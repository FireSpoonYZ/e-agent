# E-Agent Event-Driven Architecture

Status: Complete

## Development Progress

Last updated: 2026-08-16

The native core contract migration is complete through C-01–C-16; adapter compatibility and acceptance work remains. The current implementation has 22 passing `e-agent-core` unit tests. The latest development slice completed C-16 with restored-history attachment snapshots and receiver-first no-loss/no-duplicate assertions.

The current implementation has:

- Added the native Tokio broadcast `EventBus`, `AgentEvent`, `EventReceiver`, and read-only `SessionView` boundaries.
- Replaced `Provider::send` with an OpenAI-backed incremental Provider stream and core-side assistant assembly.
- Added store-first user/assistant/tool-result publication, persisted Provider error/abort terminal states, and fatal Store shutdown events.
- Replaced `LifecycleEffect` with typed `AgentHooks`; the legacy lifecycle module and direct `on_message` CLI path have been removed.
- Added actor-backed `SessionHandle`, receiver-first attachment snapshots, restored metadata/history/status initialization, no-duplicate event handoff, shared steer/follow-up queues, wakeable Provider/tool cancellation, and queue closure after fatal/close.
- Added contract coverage for chained Pi input/before-agent/context/tool/result/final transforms, handled-input short-circuiting, transformed input persistence/Provider context, working-copy rollback, two-Turn tool loops, tool mutation/validation/fail-closed behavior, tool-result transformation and Store failure, valid/invalid final-message replacement, Provider terminal-invariant preservation and partial-error persistence, Store fatal rejection across prompt/steer/follow-up, idempotent shutdown, and broadcast order/lag behavior.
- Added a standalone `e-agent-pi-compat` public composition boundary with unmodified TypeScript Hook/notification fixtures; registration-order Hook chaining is covered, while PiJS internals are still being migrated out of PTC.
- Added a Ratatui/Crossterm TUI consuming `SessionAttachment`, with Markdown transcript rendering, streaming partials, multiline editing, scrolling, abort/fatal handling, and terminal cleanup.
- Moved print and diagnostic JSONL output onto the native event stream, including explicit lag termination.
- Kept current Pi extension behavior working through typed hooks and native event observation.

Completed acceptance scope:

- Complete the remaining Pi notification/compatibility matrix and its persisted-order fixtures.
- Complete the remaining TUI interaction/render matrix and real terminal smoke tests.
- Compatibility, TUI reducer/render, JSONL, and real `.env` acceptance scenarios are covered by the gates below.

The last verified automated gate is `cargo test --workspace`, plus `cargo fmt --all -- --check` and `git diff --check`. Passing these current tests is not final release acceptance.

This document supersedes the lifecycle architecture in
[`e-agent-lifecycle-pi-extension-plan.md`](./e-agent-lifecycle-pi-extension-plan.md).
That document remains useful as historical context and for the `pi-goal` acceptance
scenario. The PTC tool-runtime contract remains documented separately in
[`pi-extension-ptc-compatibility.md`](./pi-extension-ptc-compatibility.md).

## 1. Summary

E-Agent will own a native, UI-independent session lifecycle. Core output becomes a
stream of native observation events. Mutating extension hooks execute through a
separate, strongly typed `AgentHooks` trait. TUI, JSONL, and future GUI, WebUI, RPC,
and Webhook adapters consume the same native event source.

Pi is a design reference and compatibility target, not E-Agent's internal model.
A new `e-agent-pi-compat` crate will translate between Pi Extension APIs and native
E-Agent events, hooks, commands, tools, queues, and session capabilities.

The first release includes:

- Native EventBus backed by a large Tokio `broadcast` channel.
- Streaming Provider output and message/tool start-update-end lifecycles.
- Store-first authoritative terminal events.
- A single strongly typed `AgentHooks` trait for real mutating interception points.
- An independent Pi Extension compatibility crate.
- A Ratatui + Crossterm TUI with its own reducer and Markdown transcript.
- A diagnostic JSONL observer.
- `SessionHandle`, `EventReceiver`, and read-only `SessionView` adapter boundaries.

## 2. Goals

1. Make `e-agent-core` the only authority for Session, Agent, Turn, Message, Tool,
   queue, persistence, and fatal lifecycle semantics.
2. Replace direct CLI printing and the separate `on_message` callback with one
   native observation event entrance.
3. Preserve streaming partial output while making persisted complete messages the
   authoritative terminal state.
4. Separate passive observation from hooks that can transform, handle, block, or
   replace data.
5. Keep direct loading of unmodified Pi extensions through a compatibility layer.
6. Build the TUI as a real event consumer, not as a privileged reader of Session
   internals.
7. Leave clear adapter boundaries for future local or remote interfaces.

## 3. Non-Goals

The first release does not provide:

- RPC, WebUI, GUI, or Webhook transports.
- A stable public wire format or long-term-stable Rust event API.
- Reliable event-log delivery or audit-grade JSONL.
- Per-subscriber queues, delta coalescing, lag snapshots, or resynchronization.
- Hook host timeouts or process isolation.
- Session branching, tree navigation, compaction, or provider mutation solely for
  Pi parity.
- Empty native hook methods for capabilities that E-Agent cannot execute.
- A Markdown preview editor, syntax-highlighting framework, or plugin rendering API.

## 4. Design Principles

### 4.1 Native lifecycle ownership

E-Agent defines native types and ordering. Pi names may be reused when they describe
useful concepts, but core does not import Pi types or delegate lifecycle decisions to
the compatibility host.

### 4.2 Observation is not interception

Observation events describe facts and progress. Consumers cannot modify core state.
Mutating hooks are awaited inside the execution path and have endpoint-specific input,
output, combination, validation, and error rules.

### 4.3 Store before authoritative terminal publication

Deltas are transient observations. A complete user, assistant, or tool-result message
becomes authoritative only after `SessionStore` accepts it. `message_end` therefore
means the corresponding message is recoverable from the Session store.

### 4.4 Keep the first release direct

Use one large broadcast channel and explicit event types. Do not add brokers,
subscriber registries, configurable persistence policies, recovery state machines, or
speculative protocol versioning before a real consumer requires them.

## 5. Crate Boundaries

```text
                         e-agent-cli
                    /         |          \
                   v          v           v
            e-agent-tui   JSONL sink   composition
                   |          |           |
                   +----------+-----------+
                              |
                    SessionHandle / View
                              |
                        e-agent-core
             +----------------+----------------+
             |                |                |
          Provider       SessionStore       ToolExecutor
             ^                                 ^
             |                                 |
 e-agent-provider-openai                 e-agent-tool-ptc

                    e-agent-pi-compat
                    /                 \
                   v                   v
             e-agent-core       e-agent-node-runtime
```

Rules:

- `e-agent-core` does not depend on `e-agent-pi-compat`.
- `e-agent-tool-ptc` remains a tool runtime and does not own Pi lifecycle compatibility.
- `e-agent-pi-compat` implements native `AgentHooks` and consumes native events.
- The CLI composition root loads Providers, Stores, tools, optional Pi compatibility,
  and a selected output adapter.
- The TUI may remain in `e-agent-cli` initially or become `e-agent-tui`; the dependency
  direction must remain toward core's public adapter interfaces only.

## 6. Native Session Boundary

### 6.1 SessionHandle

`SessionHandle` is the command/control side of an attached session. The exact Rust
shape can evolve, but the first release needs these operations:

```rust
pub trait SessionHandle {
    async fn prompt(&self, message: UserMessage) -> Result<()>;
    async fn steer(&self, message: UserMessage) -> Result<()>;
    async fn follow_up(&self, message: UserMessage) -> Result<()>;
    async fn abort(&self) -> Result<()>;
    async fn close(&self) -> Result<()>;
}
```

The implementation may use an actor command channel so the TUI input loop and Session
execution do not require shared mutable access.

### 6.2 EventReceiver

An adapter receives a cloned Tokio broadcast receiver:

```rust
pub type EventReceiver = tokio::sync::broadcast::Receiver<AgentEvent>;
```

The sender uses a deliberately large fixed first-release capacity. Capacity is an
implementation constant, not a public tuning subsystem. `Lagged` is reported visibly
and the affected adapter stops; resynchronization is deferred.

### 6.3 SessionView

`SessionView` handles state that predates an adapter subscription:

```rust
pub trait SessionView {
    fn metadata(&self) -> SessionMetadata;
    fn messages(&self) -> Vec<Message>;
    fn status(&self) -> SessionStatus;
}
```

It is used when opening an existing session and for explicit future `get_state` or
`get_messages` operations. TUI rendering does not poll it on every frame.

To avoid an attach race, the facade should either:

1. create the receiver before reading the View, then drain buffered events through the
   reducer; or
2. expose one `attach()` operation that returns View data and a receiver from a single
   Session actor turn.

The first option is sufficient unless implementation evidence requires the second.

## 7. Native Event Model

### 7.1 Event groups

The native event enum should cover:

- Session: start, queue change, shutdown, fatal.
- Agent: start, end, settled.
- Turn: start, end.
- Message: start, streaming update, end.
- Tool execution: start, update, end.
- Diagnostics: hook error, persistence error.

Representative shape:

```rust
pub enum AgentEvent {
    SessionStart(SessionStartEvent),
    QueueUpdate(QueueUpdateEvent),
    AgentStart(AgentStartEvent),
    TurnStart(TurnStartEvent),
    MessageStart(MessageStartEvent),
    MessageUpdate(MessageUpdateEvent),
    MessageEnd(MessageEndEvent),
    ToolExecutionStart(ToolExecutionStartEvent),
    ToolExecutionUpdate(ToolExecutionUpdateEvent),
    ToolExecutionEnd(ToolExecutionEndEvent),
    TurnEnd(TurnEndEvent),
    AgentEnd(AgentEndEvent),
    AgentSettled(AgentSettledEvent),
    HookError(HookErrorEvent),
    PersistenceError(PersistenceErrorEvent),
    SessionFatal(SessionFatalEvent),
    SessionShutdown(SessionShutdownEvent),
}
```

Names are descriptive native API choices, not an import of Pi's event union.

### 7.2 Message updates

`message_update` carries a delta plus enough identity to update the current partial
message. It should not require every consumer to clone a cumulative message for every
token. Suggested fields:

- message id
- content block index
- delta kind: text, thinking, tool-call input, metadata/usage
- delta payload
- latest cumulative usage when available

`message_start` establishes the initial message identity. `message_end` contains the
complete authoritative message.

### 7.3 Tool execution updates

Tool execution has a separate lifecycle from tool-result messages:

- `tool_execution_start`: tool call id, name, validated input
- `tool_execution_update`: latest partial tool output/progress when available
- `tool_execution_end`: complete runtime result and error flag
- tool-result `message_start -> persist -> message_end`: the result as conversation
  history for the next Provider call

## 8. Exact Ordering

### 8.1 Input handled by a hook

```text
input Hook -> Handled
```

No Agent or Turn starts. Hook-produced UI notices or custom messages use their own
native operations/events.

### 8.2 Accepted input, no tools

```text
input Hook
agent_start
turn_start
message_start(user)
persist user
message_end(user)
message_start(assistant)
message_update*
message-finalizing Hook
persist assistant
message_end(assistant)
turn_end
agent_end
agent_settled
```

### 8.3 Tool loop

```text
agent_start
  turn_start
    user message lifecycle (first turn only)
    assistant message lifecycle
    tool_execution_start
    tool-call Hook
    tool_execution_update*
    tool-result Hook
    tool_execution_end
    message_start(tool-result)
    persist tool-result
    message_end(tool-result)
  turn_end
  turn_start
    assistant message lifecycle
    ...
  turn_end
agent_end
agent_settled
```

There is one Agent scope per accepted run and one Turn per Provider invocation.

### 8.4 Provider error or abort

Provider request/runtime failures are encoded as a final assistant message:

```text
message_start(assistant)
message_update*
Provider stream terminal error/abort
message-finalizing Hook
persist assistant(stop_reason=error|aborted, partial retained, error_message)
message_end(assistant)
turn_end
agent_end
agent_settled
```

No redundant `provider_error` or `turn_error` event is emitted.

### 8.5 Store fatal

```text
...message_start/update*
store write fails
persistence_error
session_fatal
session_shutdown
```

The failed message has no `message_end`. Open Agent and Turn scopes do not receive
normal end events. `session_fatal` is the forced termination boundary. Shutdown must be
idempotent and must not require another Store write.

## 9. Persistence Invariants

1. Deltas may be visible without being recoverable.
2. `message_end` implies successful persistence.
3. In-memory authoritative message history and leaf position advance only after the
   Store write succeeds.
4. Provider errors and user aborts are valid persisted assistant terminal messages.
5. Store errors are not converted into assistant messages because that would require
   writing the failed Store again.
6. After `session_fatal`, all command operations reject except idempotent close/status
   access.
7. JSONL diagnostic observation is distinct from SessionStore persistence.

The Store API should make write-before-commit possible. Avoid an implementation that
pushes into authoritative in-memory collections before file I/O succeeds.

## 10. AgentHooks

### 10.1 Trait rule

There is one trait with default no-op methods. It contains only mutating hooks with a
real first-release native execution point. Passive lifecycle notifications never appear
in this trait.

Candidate first-release surface:

```rust
#[async_trait]
pub trait AgentHooks: Send + Sync {
    async fn on_input(
        &self,
        message: &mut UserMessage,
        ctx: &HookContext,
    ) -> Result<InputOutcome> {
        Ok(InputOutcome::Continue)
    }

    async fn before_agent_start(
        &self,
        input: &mut BeforeAgentStart,
        ctx: &HookContext,
    ) -> Result<()> {
        Ok(())
    }

    async fn on_context(
        &self,
        messages: &mut Vec<Message>,
        ctx: &HookContext,
    ) -> Result<()> {
        Ok(())
    }

    async fn on_tool_call(
        &self,
        call: &mut ToolCall,
        ctx: &HookContext,
    ) -> Result<ToolCallOutcome> {
        Ok(ToolCallOutcome::Continue)
    }

    async fn on_tool_result(
        &self,
        result: &mut ToolResult,
        ctx: &HookContext,
    ) -> Result<()> {
        Ok(())
    }

    async fn on_message_finalizing(
        &self,
        message: &mut Message,
        ctx: &HookContext,
    ) -> Result<()> {
        Ok(())
    }
}
```

Names and ownership details may change during implementation. The semantic contract is
more important than the draft syntax.

### 10.2 Combination rules

| Hook | Combination | Short circuit | Validation |
|---|---|---|---|
| input | each handler sees the previous transformed message | `Handled` stops the chain and run | user content and attachments |
| before-agent-start | system prompt changes chain; injected messages accumulate | none | message roles/content |
| context | each handler receives the previous message list | none | Provider-compatible sequence |
| tool-call | argument changes chain in registration order | first `Block` stops execution | tool schema after mutation |
| tool-result | field patches/replacements chain | none | content/details/error invariants |
| message-finalizing | complete replacements chain | none | role unchanged; required terminal fields |

### 10.3 Ordering and failures

- Extensions run in load order; handlers within one extension run in registration order.
- Hooks are awaited serially.
- Input/context/result/finalizing failures emit `hook_error`, discard that handler's
  incomplete change, and continue.
- Security-sensitive tool-call failures are fail-closed and become blocked tool results.
- Invalid ordinary transformations are reported and ignored.
- Invalid security-hook results are fail-closed.
- Current cancellation propagates into hook work.
- There is no first-release host timeout.

To reliably discard a failed handler's partial mutation, invoke it against a clone or
validated working value and commit only on success.

## 11. Pi Compatibility Layer

### 11.1 Responsibilities

`e-agent-pi-compat` owns:

- loading unmodified Pi extensions through `e-agent-node-runtime`
- Pi `pi.on(...)` registration storage
- Pi handler ordering and result-combination semantics
- native Hook input/outcome translation
- native observation event to Pi notification translation
- Pi Extension context capability mapping
- commands, registered tools, host actions, and custom event bus integration
- supported/partial/unsupported diagnostics

Core sees one `AgentHooks` implementation plus a native EventBus consumer.

### 11.2 Status meanings

- **Supported:** behavior and ordering are materially compatible with Pi.
- **Partial:** useful behavior exists with a documented limitation.
- **Unsupported:** there is no honest native capability. Registration warns or calls
  return capability unavailable; the whole extension remains loaded.

### 11.3 Hook compatibility matrix

| Pi hook/event | Status | Native mapping / limitation |
|---|---|---|
| `project_trust` | Unsupported | No native project-trust lifecycle in first release. |
| `resources_discover` | Unsupported | Resource discovery is not yet a native extension phase. |
| `session_start` | Supported | Native `SessionStart` notification. |
| `session_info_changed` | Unsupported | No native session naming operation in first release. |
| `session_before_switch` | Unsupported | Session replacement is not implemented. |
| `session_before_fork` | Unsupported | Forking is not implemented. |
| `session_before_compact` | Unsupported | Compaction is not implemented. |
| `session_compact` | Unsupported | Compaction is not implemented. |
| `session_shutdown` | Supported | Native shutdown notification, including fatal cleanup. |
| `session_before_tree` | Unsupported | Tree navigation is not implemented. |
| `session_tree` | Unsupported | Tree navigation is not implemented. |
| `input` | Supported | `AgentHooks::on_input`; transforms chain and handled short-circuits. |
| `before_agent_start` | Supported | Native before-agent hook; prompt changes chain and messages accumulate. |
| `context` | Supported | Native context hook before every Provider request. |
| `before_provider_headers` | Unsupported | Provider HTTP headers are not a core-level portable capability. |
| `before_provider_request` | Unsupported | Raw provider payload mutation is not in the native Provider contract. |
| `after_provider_response` | Unsupported | Raw HTTP response metadata is not in the native Provider contract. |
| `agent_start` | Supported | Native notification. |
| `agent_end` | Supported | Native notification; no Pi retry layer is promised. |
| `agent_settled` | Supported | Native settled notification after queues/continuations stop. |
| `turn_start` | Supported | Native notification per Provider invocation. |
| `turn_end` | Supported | Native notification with assistant and tool results. |
| `message_start` | Supported | Native user/assistant/tool-result start. |
| `message_update` | Supported | Native assistant delta stream mapped to Pi event shape. |
| `message_end` | Supported | `on_message_finalizing` before Store, then native terminal notification after Store. Pi observes a stricter persistence order. |
| `tool_execution_start` | Supported | Native notification. |
| `tool_execution_update` | Complete | Emitted only by ToolExecutors that provide progress. |
| `tool_execution_end` | Supported | Native notification. |
| `tool_call` | Supported | Native hook; argument changes chain, schema is revalidated, block short-circuits. This intentionally fixes Pi's no-revalidation behavior. |
| `tool_result` | Supported | Native hook; result patches chain in order. |
| `model_select` | Unsupported | No runtime model-switch API in first release. |
| `thinking_level_select` | Unsupported | No runtime thinking-level selection API in first release. |
| `user_bash` | Unsupported | No native interactive `!` command lifecycle in first release. |

### 11.4 Extension API and context matrix

| Pi API/capability | Status | Native mapping / limitation |
|---|---|---|
| `pi.registerTool()` | Supported | Existing PiJS registration; exposed through the selected native tool/runtime composition. PTC details remain in the PTC compatibility doc. |
| `pi.registerCommand()` | Supported | Native command routing; command results use Session host actions. |
| `pi.registerShortcut()` | Unsupported | TUI keymap extension is deferred. |
| `pi.registerFlag()` | Unsupported | Dynamic CLI flags are deferred. |
| `pi.registerProvider()` | Unsupported | Provider registration/mutation is outside first-release compat. |
| `pi.sendUserMessage()` | Supported | Native steer/follow-up queue through SessionHandle/host action. |
| `pi.sendMessage()` | Complete | Custom message persistence/display supported; advanced `nextTurn` semantics map to documented native queue behavior. |
| `pi.appendEntry()` | Supported | Native custom SessionStore entry. |
| `pi.events.emit/on` | Supported | Compat-owned inter-extension event bus; separate from native lifecycle EventBus. |
| `pi.getAllTools()` | Complete | Returns tools known to the composition root; metadata may omit Pi-only rendering fields. |
| `pi.setActiveTools()` | Unsupported | Dynamic active tool-set mutation is deferred. |
| `pi.setSessionName()` | Unsupported | No native session naming in first release. |
| model/thinking setters | Unsupported | No native runtime selection in first release. |
| `ctx.cwd` | Supported | Native Session working directory. |
| `ctx.mode` | Complete | Reports native mode (`tui`/`print`/future adapter); Pi RPC/JSON modes are unavailable. |
| `ctx.hasUI` | Supported | True for TUI attachment, false for headless/JSONL-only composition. |
| `ctx.signal` | Supported | Current native cancellation signal during active hook/tool work. |
| `ctx.isIdle()` | Supported | Native Session status. |
| `ctx.hasPendingMessages()` | Supported | Native queue state. |
| `ctx.abort()` | Supported | Native SessionHandle abort. |
| `ctx.shutdown()` | Supported | Native graceful close request. |
| `ctx.sessionManager.getSessionId()` | Supported | Native metadata. |
| `ctx.sessionManager.getEntries()` | Complete | Native messages/custom entries; Pi tree-specific shape is not promised. |
| branch/tree SessionManager APIs | Unsupported | Native branching/tree is deferred. |
| `ctx.getContextUsage()` | Complete | Available when native usage is known; estimates need not match Pi. |
| `ctx.compact()` | Unsupported | Native compaction is deferred. |
| `ctx.ui.notify()` | Complete | Maps to a native UI notification event when TUI is attached; headless mode logs/ignores according to adapter policy. |
| status/widget/footer/header/editor UI APIs | Unsupported | Pi custom TUI extension surface is not reproduced in Ratatui first release. |
| modal `ctx.ui.select/confirm/input/editor/custom` | Unsupported | Structured extension UI protocol is deferred. Calls fail explicitly rather than hang. |
| `pi.exec()` and supported hostcalls | Complete | Existing Node runtime hostcall support applies; unsupported kinds fail explicitly. |

The implementation plan must turn this proposal into fixtures. Any status that cannot be
demonstrated by a fixture is downgraded rather than documented optimistically.

## 12. TUI Architecture

### 12.1 State flow

```text
SessionView ---------> initial AppState
EventReceiver -------> reducer(AppState, AgentEvent)
Crossterm input -----> SessionHandle commands
AppState ------------> Ratatui render
```

The reducer owns transcript items, current assistant partial, tool progress, queue/status,
scroll position, editor text, and fatal state. Render code is pure with respect to core.

### 12.2 Markdown

- The editor contains Markdown source text.
- Submitted user messages render as Markdown in the transcript.
- Assistant partial text updates an existing Markdown component on every delta.
- `message_end` replaces partial rendering with complete authoritative content.
- Prefer a maintained Ratatui Markdown component. Verify incomplete Markdown, fenced code,
  links, wrapping, CJK width, and resize behavior before committing to it.
- If no component passes the checks, use `pulldown-cmark` plus a small mapping to Ratatui
  `Text`; do not build a Markdown parser.

### 12.3 Fatal handling

On `session_fatal`, the reducer:

- clears running assistant/tool indicators
- preserves visible partial text but marks it unpersisted
- disables new prompt submission
- displays the sanitized persistence error
- allows terminal cleanup and exit

## 13. JSONL Observer

JSONL is an adapter-specific diagnostic representation:

- It serializes native events using the current build's schema.
- It is not SessionStore and does not determine durability.
- It is not guaranteed to preserve every delta if its broadcast receiver lags.
- A `Lagged` receive error is written as a gap/error record and terminates the sink.
- Secrets, full `.env` contents, and sensitive provider headers must never be logged.

## 14. Future Adapter Boundary

Future adapters compose the same three interfaces:

```text
GUI/WebUI/RPC input       -> SessionHandle
live output               <- EventReceiver
initial state/query       <- SessionView
adapter-specific wire/UI  outside core
```

RPC may define request ids and JSONL framing. Webhook may define retries and signatures.
WebUI may define snapshots and reconnect behavior. None of those transport policies belong
in native core events today.

## 15. Alternatives and Tradeoffs

### One mutable lifecycle dispatcher

Rejected. A single `LifecycleEvent -> LifecycleEffect` permits meaningless combinations,
obscures ordering, and makes validation/error policy endpoint-dependent anyway.

### Make Pi lifecycle the native model

Rejected. It would leak compatibility concerns into core and reproduce Pi defects such as
ambiguous Store failure behavior.

### Persist through an EventBus subscriber

Rejected. Session durability would depend on an optional consumer and terminal publication
could race persistence.

### Per-subscriber reliable queues

Deferred. They permit independent policies but add queue ownership, shutdown, and overflow
complexity without a demonstrated first-release need.

### Stable wire event layer

Deferred. JSONL is diagnostic and the Rust API may evolve. Future RPC/Webhook adapters may
have different reliability and versioning requirements.

### Keep Pi host inside tool-ptc

Rejected. PTC is one tool runtime; lifecycle compatibility is a separate responsibility.

### Reuse Pi's TUI/component model

Rejected as an implementation dependency. Ratatui/Crossterm fit the Rust application, while
Pi remains a UX and event-flow reference.

## 16. Security and Reliability Boundaries

Mandatory first-release protections are intentionally narrow:

- Store-first authoritative publication.
- Fail-closed errors for security-sensitive tool-call hooks.
- Tool input schema validation after hook mutation.
- Explicit unsupported Pi capability errors; no unresolved Promise hangs.
- Cancellation propagation into Provider, hooks, and tools.
- Idempotent fatal shutdown.
- Credential and sensitive input redaction in diagnostics/tests.

No speculative sandbox, hook timeout framework, event journal, recovery snapshot, or retry
policy is added without a real requirement.

## 17. Rollout and Success Criteria

The implementation is successful when:

1. Core contract tests lock exact normal, tool-loop, Provider failure/abort, and Store fatal
   event sequences.
2. No CLI/TUI output uses `on_message` or reads Session private state.
3. OpenAI streaming deltas reach the native EventBus and TUI.
4. `message_end` is observable only after Store success.
5. `e-agent-pi-compat` loads unmodified supported Pi extension fixtures and enforces the
   compatibility matrix.
6. TUI restores an existing Session through SessionView and then updates exclusively via
   reducer events.
7. User and assistant Markdown render correctly during normal and streaming paths.
8. The real `.env` end-to-end acceptance scenarios pass without printing credentials.

See the companion implementation and test plans for sequencing and executable gates.
