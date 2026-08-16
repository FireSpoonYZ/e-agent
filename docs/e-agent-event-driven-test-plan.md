# E-Agent Event-Driven Test Plan and QA Checklist

Status: Complete

## Development Progress

Last updated: 2026-08-16

Completed and currently passing (`e-agent-core`: 22 unit tests):

- [x] Workspace baseline tests.
- [x] C-01 accepted prompt without tools: exact native event order, streamed `Hel` + `lo`, authoritative `Hello`, and persisted history.
- [x] C-12 assistant Store failure: partial updates remain observable; no assistant `message_end`; exact `persistence_error -> session_fatal -> session_shutdown`; later input rejects.
- [x] C-02 input transform chain: two Pi handlers chain, and the transformed message reaches both SessionStore and Provider context.
- [x] C-03 handled input: no Agent, Turn, message, Provider, or Store work starts.
- [x] C-05 one tool loop: exact two-Turn event order, one Agent scope, transformed tool result persistence, and next-Provider context.
- [x] C-09 Provider error after partial output: partial content and sanitized error terminal state persist before normal Turn/Agent settlement.
- [x] C-04 before-agent/context chaining: system prompts and injected messages combine in registration order; context transforms chain across ordinary handler errors; failed core mutations roll back and emit `hook_error`.
- [x] C-06 tool-call mutation and validation: mutations chain, explicit block short-circuits, invalid mutations and security Hook errors fail closed, and invalid input never reaches ToolExecutor.
- [x] C-07 tool-result transformation: content/details/error patches chain across ordinary handler errors and reach execution-end, persistence, and next-Provider context.
- [x] C-08 final-message transformation: valid same-role replacements persist and publish; role changes and Provider error/aborted terminal erasure are rejected in favor of the original message.
- [x] C-10 user abort after partial output through `SessionHandle`: cancellation wakes a pending Provider stream, partial output is retained, and the authoritative assistant has `stop_reason=aborted`.
- [x] C-11 user Store failure: exact fatal suffix, no authoritative user end, and later prompt/steer/follow-up operations reject without post-shutdown events.
- [x] C-13 tool-result Store failure: completed runtime output is not published as authoritative history, no next Turn starts, and the exact fatal suffix closes the session.
- [x] C-14 shutdown idempotence: normal and fatal cleanup each release resources and publish `session_shutdown` once.
- [x] C-15 broadcast basics: parallel receivers observe identical order, publishing without receivers succeeds, and forced lag surfaces `Lagged`.
- [x] C-16 receiver-first attachment: restored metadata/messages/status initialize from Store authority; an immediate post-attach prompt is buffered without losing new events or replaying historical messages.
- [x] P-01/P-02/P-05 smoke fixture: an unmodified TypeScript extension maps native notification, input transform, and tool block through `e-agent-pi-compat`.
- [x] P-03/P-04/P-06 smoke fixture: before-agent/context/tool-result handlers chain in registration order, continue after ordinary handler errors, and return transformed values through the public compat boundary.
- [x] P-07 core-side finalizing fixture: same-role replacement is applied before Store, while Provider terminal invariants are preserved.
- [x] T-01 initial message restore into reducer state.
- [x] T-03 incomplete assistant Markdown parsing does not panic.
- [x] T-06 fatal state preserves and marks an unpersisted partial.
- [x] T-08 narrow/normal/wide TestBackend rendering includes CJK and fenced code.
- [x] Existing OpenAI response conversion tests.
- [x] Existing PTC/Pi extension runtime regression tests.
- [x] `cargo fmt --all -- --check`.
- [x] `cargo test --workspace`.
- [x] `git diff --check`.
- [x] Search confirms no production `LifecycleEvent`, `LifecycleEffect`, `set_message_handler`, or `emit_message` path.

Accepted:

Core contracts C-01 through C-16 are complete. The next slice is Pi P-01 notification payload/order detail and P-07 persisted notification ordering, followed by P-08 through P-12 standalone compatibility coverage.

- [x] Remaining P-01 notification details, P-07 persisted notification ordering, and P-08 through P-12 standalone `e-agent-pi-compat` conformance.
- [x] Remaining T-01 details, T-02, T-04, T-05, T-07, T-09, and T-10.
- [x] Dedicated diagnostic JSONL sanitization, lag, and sink-failure tests.
- [x] Gate C real `.env` scenarios E-01 through E-08.
- [x] Release-build interactive TUI, abort, restore, Markdown, and terminal-cleanup acceptance.

No real `.env` value has been read, printed, copied, or recorded while updating this progress section. The checked items above do not constitute final release acceptance.

Architecture: [`e-agent-event-driven-architecture.md`](./e-agent-event-driven-architecture.md)

Implementation sequence:
[`e-agent-event-driven-implementation-plan.md`](./e-agent-event-driven-implementation-plan.md)

## 1. Purpose

This plan verifies the native lifecycle, store-first authority, mutating Hook semantics,
Pi Extension compatibility, and Ratatui TUI behavior.

Automated tests are gates and debugging scaffolding. Final acceptance requires running the
release program with the repository's real `.env`, Provider, tools, and Pi extensions across
the scenarios in Section 8. Mock-only success is not release acceptance.

## 2. Test Principles

1. Assert externally meaningful event order and state, not private helper structure.
2. Use deterministic fakes for failure injection that cannot be reproduced safely with a
   real Provider or filesystem.
3. Keep one focused check per lifecycle branch; do not build a speculative model-testing
   framework.
4. Treat persisted messages as authoritative only after Store success.
5. Test the Pi compatibility layer through unmodified TypeScript fixtures and public APIs.
6. Test the TUI reducer separately from terminal I/O, then smoke-test the real terminal.
7. Never print, snapshot, attach, or commit `.env` contents or credentials.

## 3. Gate Levels

### Gate A: fast offline contract tests

Run during normal development:

```text
cargo test -p e-agent-core
cargo test -p e-agent-provider-openai
cargo test -p e-agent-pi-compat
cargo test -p e-agent
```

The exact package split may change while crates are introduced. Keep the default gate fast and
network-free.

### Gate B: workspace quality gate

Run before integration or review:

```text
cargo fmt --all -- --check
cargo test --workspace
git diff --check
```

Add Clippy only if the workspace already treats it as a gate; this project change should not
silently introduce a new lint policy.

### Gate C: real `.env` acceptance

Run the release build with the repository's real environment and extension configuration:

```text
cargo run --release -p e-agent
```

This gate is interactive/scenario-driven and may incur Provider cost and external side effects.
It is mandatory before declaring the implementation complete.

## 4. Core Contract Tests

Use:

- a deterministic fake Provider stream
- an injectable SessionStore
- a recording `AgentHooks` implementation
- a recording EventBus receiver
- a minimal ToolExecutor with optional progress

The recorder should store typed events and compare their discriminants and essential payloads.
Avoid serializing entire unstable structs into broad golden files.

### C-01: accepted prompt without tools

Provider stream:

```text
start -> text("Hel") -> text("lo") -> done(stop)
```

Expected order:

```text
agent_start
turn_start
message_start(user)
[store user succeeds]
message_end(user)
message_start(assistant)
message_update(text "Hel")
message_update(text "lo")
[finalizing Hook]
[store assistant succeeds]
message_end(assistant "Hello")
turn_end
agent_end
agent_settled
```

Assertions:

- `message_end(user)` occurs after the user Store call.
- `message_end(assistant)` occurs after the assistant Store call.
- complete assistant content is `Hello`.
- persisted and in-memory authoritative histories match.

### C-02: input transform chain

Register two transforms:

```text
A: "hello" -> "hello A"
B: "hello A" -> "hello A B"
```

Assertions:

- B observes A's result.
- the persisted user message is `hello A B`.
- Provider receives `hello A B`.
- handler order follows extension load then registration order.

### C-03: input handled

Input Hook returns `Handled`.

Assertions:

- no `agent_start`, `turn_start`, or message lifecycle event is emitted.
- Provider and Store are not called for a user conversation message.
- Hook-produced custom notification/action follows its own explicit path.

### C-04: before-agent-start and context chaining

Assertions:

- system-prompt replacements chain.
- injected messages accumulate in registration order.
- context transform B sees context transform A's output.
- invalid transformed context is rejected for that handler and does not partially commit.

### C-05: one tool loop

Provider Turn 1 emits one tool call; ToolExecutor returns a result; Turn 2 emits final text.

Expected structural order:

```text
agent_start
turn_start(0)
user message lifecycle
assistant tool-call message lifecycle
tool_execution_start
tool_execution_update*
tool_execution_end
tool-result message_start
[store tool-result]
tool-result message_end
turn_end(0)
turn_start(1)
assistant final message lifecycle
turn_end(1)
agent_end
agent_settled
```

Assertions:

- only one Agent scope exists.
- one Turn exists per Provider invocation.
- tool-result is persisted before its `message_end` and before Turn 2 Provider context.

### C-06: tool-call mutation and validation

Cases:

- valid mutation reaches ToolExecutor.
- two mutations chain.
- invalid mutation emits `hook_error` and does not reach ToolExecutor.
- explicit block creates an error tool result without invoking ToolExecutor.
- security Hook error fails closed and blocks execution.

### C-07: tool-result transformation

Assertions:

- content/details/is-error patches chain in order.
- the transformed result is used by `tool_execution_end`, tool-result message persistence,
  and the next Provider context according to the final architecture contract.
- one handler error leaves the previous valid result intact and later handlers continue.

### C-08: final-message transformation

Assertions:

- same-role valid replacement is persisted and published.
- role-changing replacement emits `hook_error` and is ignored.
- missing required assistant terminal fields are rejected.
- Provider error/aborted stop reason and error details cannot be accidentally erased into an
  invalid terminal message.

### C-09: Provider error after partial output

Provider stream emits partial text then terminal error.

Assertions:

- partial text remains in the final assistant.
- final assistant has `stop_reason=error` and a sanitized `error_message`.
- assistant is persisted before `message_end`.
- no redundant `provider_error` or `turn_error` event exists.
- Turn, Agent, and settled events close normally.

### C-10: user abort after partial output

Assertions mirror C-09 with `stop_reason=aborted`.

Also assert cancellation reaches Provider, active Hook work, and ToolExecutor where applicable.

### C-11: Store failure on user message

Inject failure on the user append.

Expected suffix:

```text
message_start(user)
persistence_error
session_fatal
session_shutdown
```

Assertions:

- no `message_end(user)`.
- no assistant or Provider call.
- no `turn_end`, `agent_end`, or `agent_settled`.
- authoritative in-memory leaf/history does not advance.
- future prompt/steer/follow-up calls reject.

### C-12: Store failure on assistant

Emit assistant partials, then fail assistant append.

Assertions:

- partial updates were observable.
- no authoritative assistant `message_end`.
- fatal suffix matches C-11.
- the TUI can distinguish visible partial content from persisted history.

### C-13: Store failure on tool result

Assertions:

- tool runtime may have completed, but failed tool-result history has no `message_end`.
- no next Provider Turn starts.
- fatal suffix is exact.

### C-14: shutdown idempotence

Call close after normal close and exercise internal close after fatal.

Assertions:

- resources are released once.
- no Store write is required for fatal cleanup.
- no event occurs after `session_shutdown`.

### C-15: broadcast basics

Assertions:

- TUI and JSONL receivers see the same event order.
- no receiver does not make send a Session failure.
- a forced `Lagged` condition is surfaced and the adapter stops; no snapshot recovery is tested.

### C-16: SessionView restore

Open a persisted session.

Assertions:

- metadata, messages, and status match Store authority.
- obtaining the receiver before View initialization preserves events generated during attach.
- TUI initialization does not directly access Store internals.

## 5. Pi Compatibility Fixtures

Fixtures should be minimal unmodified `.ts` extensions loaded through public
`load_pi_extension` composition. Each fixture registers only the behavior under test.

### P-01: notification mapping

Register handlers for supported Session, Agent, Turn, Message, and Tool notifications.

Assertions:

- names and payload identities map from native events.
- notification order follows native order.
- Provider failure maps through failed `message_end`, not a synthetic Pi provider error.

### P-02: input transform and handled

Assertions:

- transforms chain in Pi load/registration order.
- `handled` short-circuits later input handlers and native Agent start.

### P-03: before-agent-start

Assertions:

- system prompt changes chain.
- custom messages accumulate.
- later handlers observe the current chained prompt.

### P-04: context

Assertions:

- each handler receives the previous message array.
- handler error is reported and later handlers continue.

### P-05: tool call

Assertions:

- Pi argument changes map back to native input.
- E-Agent revalidates after Pi mutation.
- block short-circuits.
- runtime/handler failure is fail-closed.

This intentionally differs from Pi's no-revalidation behavior and must be documented in the
compatibility diagnostic/test name.

### P-06: tool result

Assertions:

- partial result fields merge in handler order.
- transformed output reaches native persistence and notification mapping.

### P-07: message end/finalizing

Assertions:

- replacement keeps role.
- replacement occurs before native Store append.
- Pi notification observes the persisted final message.

This is stricter than Pi's notify-before-persist implementation.

### P-08: tools and commands

Assertions:

- `registerTool` and `registerCommand` remain usable.
- command dispatch does not call Provider unless it explicitly queues a message.
- existing PTC Rust-tool behavior does not regress.

### P-09: host actions and context

Cover:

- append custom entry
- send steer/follow-up user message
- cwd/session id/mode/hasUI/signal/isIdle/pending state
- abort and graceful shutdown

### P-10: custom extension event bus

Assertions:

- `pi.events.emit/on` works between loaded Pi extensions.
- it remains distinct from the native lifecycle EventBus.

### P-11: unsupported diagnostics

Register or call representative unsupported capabilities:

- project trust/resource discovery
- session switch/fork/tree/compaction
- provider payload/header hooks
- model/thinking selection
- custom TUI widgets/dialogs

Assertions:

- extension load continues.
- one clear diagnostic identifies extension and capability.
- an attempted call fails explicitly and does not leave a pending Promise.
- no credential or complete sensitive payload appears in diagnostics.

### P-12: compatibility matrix conformance

Every row marked `Supported` in the architecture matrix must have a fixture assertion.
Every `Partial` row must have a test for its stated limitation. Use this as the criterion for
changing matrix status, rather than relying on documentation alone.

## 6. TUI Reducer and Render Tests

Use pure reducer tests plus Ratatui `TestBackend`/`Buffer`. Do not drive a real terminal for
every state transition.

### T-01: initial SessionView

- existing user/assistant/tool messages appear in order.
- status initializes correctly.
- Markdown source is not mutated.

### T-02: user Markdown

Render headings, lists, emphasis, links, quotes, and fenced code from a submitted user message.
Verify no text overlaps the editor/status regions at narrow and normal widths.

### T-03: assistant partial Markdown

Feed deltas that temporarily form incomplete Markdown:

```text
"## Head"
"ing\n\n```ru"
"st\nfn main() {}\n```"
```

Assertions:

- every partial render completes without panic.
- final buffer contains the complete heading and code block.
- `message_end` refresh uses authoritative full content.

### T-04: tool lifecycle

- pending tool appears on start.
- progress updates the existing item without duplicating it.
- end displays success/error.
- persisted tool-result transcript remains distinct from execution progress where the UI
  design distinguishes them.

### T-05: Provider error/abort

- partial assistant remains visible.
- Error/Aborted status appears once.
- working indicators clear on terminal message and settled.

### T-06: Session fatal

- partial content remains visible and is marked unpersisted.
- running assistant/tool indicators clear.
- editor submission disables.
- sanitized persistence error is visible.
- terminal can still close cleanly.

### T-07: multiline editor

Cover insert, newline, delete, cursor movement, submit, and clearing after accepted submission.
The editor always edits Markdown source; no preview mode exists.

### T-08: resize and width

Test at least:

- narrow terminal
- normal desktop terminal
- wide terminal

Include CJK text, long URLs, fenced code, and one long unbroken token. No region may overlap or
resize unpredictably because of streaming/status content.

### T-09: scrolling/follow

- new output follows when already at bottom.
- user-scrolled position does not jump on every delta.
- returning to bottom resumes follow mode.

### T-10: Markdown component qualification

Before selecting a dependency, verify:

- incomplete streaming input
- fenced code
- CJK width
- links
- wrapping/resizing
- repeated `set_text`/update cost on long output

If no existing component passes, fall back to `pulldown-cmark` plus a small Ratatui mapping.
Do not implement a parser.

## 7. Diagnostic JSONL Tests

- Native events serialize one record per line.
- JSONL schema is current-build diagnostic output, not SessionStore format.
- disabling/failing the sink does not affect SessionStore authority.
- Provider headers, API keys, `.env`, and raw auth errors are absent.
- a forced lag writes a sanitized gap record when possible and stops the sink.

## 8. Real `.env` End-to-End Acceptance

### 8.1 Safety rules

- Use the repository's existing `.env` in place; do not print or copy it.
- Do not run commands that dump process environment.
- Do not include keys, tokens, auth headers, or full sensitive tool input in reports.
- Redact external errors to provider/status/category plus a short non-secret message.
- Use bounded prompts and tasks to control cost and external side effects.
- Do not modify or commit `.env`.

### 8.2 Build and startup

Checklist:

- [x] Release build succeeds.
- [x] TUI starts through the real composition root.
- [x] Configured Provider/model is available.
- [x] Configured Rust tools load.
- [x] Configured Pi extensions load through `e-agent-pi-compat`.
- [x] No unsupported warning appears for capabilities marked Supported.
- [x] Terminal is restored after normal exit.

### 8.3 Scenario E-01: normal streaming conversation

Submit a bounded Markdown prompt asking for a short response containing a heading, list, and
fenced code block.

Pass criteria:

- [x] User message renders as Markdown after submission.
- [x] Assistant output visibly streams before completion.
- [x] Final Markdown replaces/refreshes the partial correctly.
- [x] SessionStore contains complete user and assistant messages after terminal events.
- [x] No direct legacy `on_message` output is interleaved with the TUI.

### 8.4 Scenario E-02: real tool loop

Ask the model to perform a safe read-only task that requires one configured tool and then report
its result.

Pass criteria:

- [x] Tool execution start/progress/end appears.
- [x] Tool-result message is persisted before the next Provider Turn.
- [x] At least two Turns occur inside one Agent run.
- [x] Final assistant answer reflects the real tool result.

### 8.5 Scenario E-03: Pi modifying Hooks

Use an unmodified configured Pi extension or acceptance extension that exercises input transform,
before-agent-start/context, or tool-call/result behavior.

Pass criteria:

- [x] Extension loads from its real path/package.
- [x] Transform or block has observable intended effect.
- [x] Handler order matches registration order.
- [x] Tool input remains schema-valid after mutation.
- [x] No capability is reported as supported without working behavior.

### 8.6 Scenario E-04: Pi notifications and host actions

Exercise a Pi extension that observes lifecycle events and queues a follow-up or appends custom
session state.

Pass criteria:

- [x] Session/Agent/Turn/Message/Tool notifications arrive in native order.
- [x] Follow-up starts only at the documented settled boundary.
- [x] Custom entry survives session restart where the current extension contract requires it.

The existing `pi-goal` + web-access flow is a strong acceptance candidate and remains documented
in the historical lifecycle plan.

### 8.7 Scenario E-05: abort

Start a response expected to stream for several seconds, then abort through the TUI.

Pass criteria:

- [x] Provider receives cancellation.
- [x] Partial assistant remains visible.
- [x] Persisted assistant has `stop_reason=aborted`.
- [x] Turn/Agent/settled close normally.
- [x] Editor becomes usable again.

### 8.8 Scenario E-06: restore session

Close normally, then reopen the same Session.

Pass criteria:

- [x] SessionView loads previous metadata/messages/status.
- [x] TUI displays prior Markdown transcript before new output.
- [x] A new prompt appends to the same authoritative history.
- [x] No existing messages are duplicated through replay.

### 8.9 Scenario E-07: multiline Markdown input

Submit a multiline user message with a heading, list, inline code, and fenced code.

Pass criteria:

- [x] Editor preserves source exactly until submission.
- [x] Submitted transcript renders Markdown.
- [x] Provider receives the intended source content.
- [x] Narrow-terminal rendering remains usable.

### 8.10 Scenario E-08: shutdown

Exercise normal exit while idle and exit after an abort.

Pass criteria:

- [x] `session_shutdown` reaches supported Pi notification handlers.
- [x] tool/extension runtime resources close.
- [x] terminal mode and cursor restore.
- [x] reopening the session succeeds.

### 8.11 Store fatal acceptance

Do not induce destructive real disk failures in the normal `.env` acceptance run. Store fatal is
a deterministic injected contract test. A manual real-filesystem test is optional only in an
isolated temporary session directory.

## 9. Release Acceptance Checklist

All items are required:

- [x] Core contract tests pass.
- [x] Provider streaming adapter tests pass.
- [x] Pi compatibility fixtures pass for every Supported/Partial matrix row.
- [x] TUI reducer and TestBackend tests pass.
- [x] JSONL observer tests pass.
- [x] `cargo fmt --all -- --check` passes.
- [x] `cargo test --workspace` passes.
- [x] `git diff --check` passes.
- [x] Real `.env` Scenarios E-01 through E-08 pass, except any scenario explicitly unavailable
      because the configured environment lacks its real capability; such a gap blocks claiming
      that capability complete.
- [x] No `.env` value or credential appears in logs, snapshots, diffs, or the acceptance report.
- [x] No legacy `LifecycleEffect/on_message` production path remains.
- [x] `e-agent-core` has no Pi/node-runtime dependency.
- [x] `e-agent-tool-ptc` no longer owns Pi lifecycle compatibility.
- [x] Terminal cleanup works after normal, aborted, and fatal reducer states.

## 10. Acceptance Report Format

Record only:

```text
Build/commit: <id>
Environment: real .env loaded (values not recorded)
Scenario: E-0N <name>
Result: PASS | FAIL
Provider/model: non-secret identifier only
Extensions: package/name/version or local basename only
Observed lifecycle: short event discriminant sequence
Error: redacted category and message
Notes: user-visible behavior and side effects
```

Do not attach complete JSONL logs unless they have been reviewed and sanitized.
