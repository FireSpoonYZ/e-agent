# Repository integration boundaries for a replaceable TUI and Pi extension UI

## Scope and source state

This report inspects the current repository only. The active implementation is the workspace member `e-agent-pi-compat`; `e-agent-tool-ptc` remains a separate older/native PTC implementation. The worktree already contains an uncommitted modification at `e-agent-tool-ptc/src/ptc/mod.rs` (`git status --short`), so migration must not overwrite or use that file as a staging area.

## Executive findings

1. **The core/session boundary is already suitable for replaceable frontends.** `SessionAttachment` gives one command client, one ordered event receiver, initial metadata/history/status, and an attach-race safeguard (`e-agent-core/src/session/handle.rs:16-39`, `e-agent-core/src/session/handle.rs:100-148`). No Ratatui, Crossterm, or Pi dependency points into core (`e-agent-core/Cargo.toml:6-19`).
2. **The concrete TUI is not internally replaceable.** Terminal ownership, Crossterm decoding, event scheduling, reducer, editor policy, layout, Markdown conversion, rendering, and tests all live in `e-agent-tui/src/lib.rs` (`:1-26`, `:28-241`, `:243-348`, `:350-755`, `:757-954`). The CLI hard-codes its entry point (`e-agent-cli/src/main.rs:104-107`).
3. **Pi TypeScript UI is partially modeled in the JS runtime but disconnected from interactive mode.** The runtime can emit typed `HostcallKind::Ui { op }` requests (`e-agent-node-runtime/src/runtime.rs:175-207`) and contains shims for dialogs, notifications, status, widget/title/editor text, themes, and a polling custom-component approximation (`e-agent-node-runtime/src/runtime.rs:21277-21630`). However, `PiCompat` always supplies `mode="print", hasUI=false` (`e-agent-pi-compat/src/lib.rs:478-485`, `:792-795`), and its hostcall dispatcher merely acknowledges four fire-and-forget operations without forwarding them (`e-agent-pi-compat/src/lib.rs:1056-1064`). Therefore unchanged extensions never reach the Rust TUI.
4. **Do not put extension UI into `AgentEvent`.** Native events are authoritative session facts (`e-agent-core/src/event.rs:30-105`), whereas UI calls include request/reply dialogs, ephemeral decorations, terminal input, and renderer registrations. A separate versioned UI contribution/request port is required at the composition layer. This preserves the architecture rule that core owns lifecycle and frontends consume events (`docs/e-agent-event-driven-architecture.md:91-113`, `:115-149`).
5. **The minimum seam is composition-root wiring plus a UI broker/adapter, not a rewrite of session orchestration.** The CLI can construct the chosen TUI implementation and a Pi-compat UI adapter before attaching the session. A second TUI should consume the same attachment and UI protocol without edits to core.

## Current dependency direction

Workspace manifests establish this direction:

```text
e-agent-cli
  -> e-agent-core
  -> e-agent-pi-compat -> e-agent-core + e-agent-node-runtime
  -> e-agent-tui      -> e-agent-core + Ratatui + Crossterm
  -> provider

e-agent-core -> e-agent-extension (session/tool identity and ABI types)
e-agent-node-runtime -> no core/TUI dependency
e-agent-tool-ptc -> core + node-runtime (separate legacy/native PTC path)
```

Evidence:

- Workspace members: `Cargo.toml:1-3`.
- CLI dependencies: `e-agent-cli/Cargo.toml:6-16`.
- TUI depends inward only on core and its concrete rendering stack: `e-agent-tui/Cargo.toml:6-15`.
- Pi compatibility depends on core and the JS runtime, not the TUI: `e-agent-pi-compat/Cargo.toml:6-17`.
- Node runtime has no core or TUI dependency: `e-agent-node-runtime/Cargo.toml:9-37`.
- The intended architecture already says core must not depend on Pi compatibility, PTC must not own Pi lifecycle, and CLI is the composition root (`docs/e-agent-event-driven-architecture.md:115-149`).

This direction should remain. In particular, neither Ratatui types nor Pi/ANSI/raw-input compatibility types should enter `e-agent-core`.

## Reusable native contracts

### Session command side

`SessionHandle` already expresses stable frontend commands: `prompt`, `steer`, `follow_up`, `abort`, and `close` (`e-agent-core/src/session/handle.rs:16-23`). `SessionClient` implements these through an actor channel and shared queue/cancellation handles (`:25-31`, `:47-98`). This is reusable by any TUI implementation.

The concrete actor is local-task based: `Session::attach` calls `tokio::task::spawn_local` (`e-agent-core/src/session/handle.rs:100-135`). Consequently the caller must run it in a `LocalSet`; the CLI currently does so only around `e_agent_tui::run` (`e-agent-cli/src/main.rs:104-107`). The selected replacement runner must retain that runtime requirement or the CLI must own the `LocalSet` around a generic frontend runner.

### Initial snapshot and live event side

`SessionAttachment` contains `SessionClient`, `EventReceiver`, metadata, restored messages, and status (`e-agent-core/src/session/handle.rs:33-39`). Attach subscribes **before** collecting the snapshot so events emitted during adapter initialization remain buffered (`:105-115`), then moves the session into its actor (`:116-148`). Core tests lock restored history, buffered events, prompt/close behavior, and abort persistence (`e-agent-core/src/session/mod.rs:1776-1846`, `:1849-1895`).

`AgentEvent` covers session, queue, agent, turn, message streaming, tool execution, diagnostics, fatal, and shutdown (`e-agent-core/src/event.rs:30-105`). The bus is a fixed-capacity Tokio broadcast channel of 16,384 (`:7-28`); tests verify equal order across receivers and explicit lag (`e-agent-core/src/session/mod.rs:2172-2208`).

Important semantics to preserve in any projection:

- `MessageEnd` is store-first and authoritative (`e-agent-core/src/session/mod.rs:290-323`; `docs/e-agent-event-driven-architecture.md:103-107`).
- Store fatal publishes persistence error, fatal, and shutdown, then rejects further work (`e-agent-core/src/session/mod.rs:274-287`).
- `SessionStatus` is only `Idle`, `Running`, `Fatal`, `Closed` (`e-agent-core/src/session/mod.rs:43-50`); extension status/widgets are separate UI state, not session status.
- A lagged frontend currently cannot resnapshot through `SessionClient`; the current TUI correctly treats lag as fatal-visible adapter failure (`e-agent-tui/src/lib.rs:255-267`). A replacement contract must either preserve this policy initially or later add an explicit resync capability without pretending broadcast is reliable.

### Existing TUI projection and deterministic checks

`AppState::new` restores authoritative messages/status (`e-agent-tui/src/lib.rs:38-72`), and `reduce` projects streaming assistant/thinking/tool state and fatal/shutdown behavior (`:74-164`). This reducer logic is valuable, but it is currently coupled in the same module to editor/scroll fields and renderer-specific state (`:38-53`).

Existing behavior is evidenced by tests for:

- stream/fatal projection: `e-agent-tui/src/lib.rs:763-796`;
- narrow/normal/wide and CJK rendering: `:798-815`;
- terminal-width cursor placement: `:817-822`;
- Markdown tables and incomplete Markdown: `:824-852`;
- scroll/follow behavior: `:855-892`;
- finalized thinking/tool source: `:894-938`;
- multiline editing/delete/vertical movement: `:939-953`.

These should become contract fixtures rather than be discarded.

## Concrete TUI coupling and likely module seams

Current concentration points:

- Crossterm event and lifecycle imports: `e-agent-tui/src/lib.rs:3-11`.
- Ratatui layout/style/widget imports: `:18-25`.
- Projection, editor, scrolling: `:28-241`.
- lifecycle/event loop/render tick: `:243-282`.
- input policy and session effects: `:284-348`.
- layout and cursor/IME location: `:350-402`.
- transcript/Markdown/tool rendering: `:404-740`.
- alternate-screen/raw-mode guard: `:742-755`.

A minimal coherent split is:

```text
e-agent-tui (renderer-neutral interactive application contract)
  attachment.rs      consumes SessionAttachment / exposes session effects
  state.rs           UI projection + reducer
  input.rs           normalized key/mouse/paste/resize events and commands
  components.rs      width-constrained component/view/focus contracts
  ui_protocol.rs     versioned extension UI requests/contributions/capabilities
  runner.rs          scheduler/invalidation and lifecycle coordination

e-agent-tui-ratatui (or a ratatui module retained temporarily)
  terminal.rs        Crossterm lifecycle/capabilities/input decoding
  render.rs          Ratatui buffers/layout/cursor
  markdown.rs        current pulldown-cmark mapping
  components/*       reference editor/dialog/select/transcript implementations

e-agent-cli
  selects/constructs frontend and Pi UI adapter
```

Whether the Ratatui implementation becomes a new crate immediately is a rollout choice. The **essential** compile-time seam is a runner/component contract selected by CLI; moving files without that seam achieves little. Conversely, a one-implementation `Terminal` trait is not justified unless it enables deterministic fake-terminal testing or the second implementation. Traits should exist only where replacement/test reuse is concrete: runner, terminal driver, renderer/component host, and extension UI broker.

The old `pub async fn run(SessionAttachment)` can remain as a compatibility wrapper that constructs the Ratatui/Crossterm reference stack (`e-agent-tui/src/lib.rs:243-282`).

## Existing Pi extension runtime and partial UI hostcalls

### What already exists and should be reused

The node runtime already has a renderer-neutral hostcall envelope:

- `HostcallKind::Ui { op: String }`: `e-agent-node-runtime/src/runtime.rs:175-192`.
- request correlation, payload, trace, and extension identity: `:194-207`.
- capability classification marks UI as `"ui"`: `:293-318`.
- canonical request hashing includes UI op and payload: `:367-395`.

The JS compatibility shim already emits:

- `select`, `confirm`, `input`, `editor`: `e-agent-node-runtime/src/runtime.rs:21298-21334`;
- `notify`, `setStatus`, footer/header/working aliases, widgets, title, editor text/get: `:21335-21401`;
- `custom()` with width-aware render frames, input polling, disposal, and close: `:21402-21593`;
- theme listing/get/set and a minimal theme object: `:21277-21286`, `:21595-21630`;
- context chooses the UI template from `hasUI`: `:21632-21670`.

The runtime test `pijs_custom_ui_width_updates_trigger_reflow` proves correlated `custom` polling plus `setWidget` frame emission and width-driven rerender (`e-agent-node-runtime/src/runtime.rs:27942-28075`). This is a useful compatibility fixture but not an end-to-end TUI test.

The runtime also stores registrations that currently never reach the app:

- shortcuts: `e-agent-node-runtime/src/runtime.rs:20949-20973`;
- custom message renderers: `:20975-20992`;
- snapshots include shortcuts and message renderer names: `:21231-21265`;
- the public JS object exposes `registerShortcut` and `registerMessageRenderer`: `:22527-22549`.

### What is missing or misleading

`PiCompat::pi_context` always reports print/headless (`e-agent-pi-compat/src/lib.rs:478-485`). Tool execution independently does the same (`:792-795`). Interactive CLI selection is never communicated into `PiCompat`, because the tool/extension host is moved into `Session` before TUI selection (`e-agent-cli/src/main.rs:36-68`, `:93-107`). Thus every unchanged extension sees `ctx.hasUI === false`, despite the documented Pi interactive contract being `mode="tui", hasUI=true` (`extensions.md:2897-2906`).

When a UI hostcall does occur, `execute_hostcall_with_state` only returns success for `notify`, `setStatus`, `setWidget`, and `setTitle`; it drops payloads and creates no native action (`e-agent-pi-compat/src/lib.rs:1056-1064`). Modal calls fall through to the generic hostcall dispatcher and become unsupported errors (`:1067-1125`). There is no request/reply channel to the TUI.

The compatibility matrix explicitly calls `registerShortcut` and custom UI unsupported (`e-agent-pi-compat/src/lib.rs:1391-1406`), with a test asserting custom UI remains unsupported (`:1510-1523`). That test and matrix must be versioned/replaced when the bridge lands; silently changing the meaning would make compatibility reporting unreliable.

The repository architecture documentation is also stale/internally inconsistent: it claims `ctx.hasUI` is supported and `notify` maps to a native event (`docs/e-agent-event-driven-architecture.md:546-561`), while production code always reports false and drops notifications. Treat code as authoritative and record this as a migration defect.

### Gaps relative to the documented pinned UI surface

The checked-in Pi documentation requires dialogs, notifications, status/working indicators, widgets above/below editor, footer, title, editor text/paste, autocomplete providers, tool expansion, replaceable editor, themes, and custom components (`extensions.md:2472-2652`). Custom overlays require focus/visibility handles (`extensions.md:2701-2764`); custom editors require component factory composition and keybindings (`:2766-2815`). Interactive mode semantics are explicit (`:2897-2906`).

The JS shim only partially covers these. Missing or incomplete items include:

- timeout/AbortSignal dialog cancellation and exact return defaults;
- working visibility/indicator, header/footer component factories, widget placement/component factories;
- paste-to-editor, autocomplete providers, tool expansion;
- editor component get/set/composition;
- real theme styling/cache invalidation (current theme strips styles to text at `runtime.rs:21277-21286`);
- overlay handles/focus/visibility and IME ownership;
- terminal input subscriptions and text clipboard operations;
- invocation of registered shortcuts;
- exporting/invoking custom message renderers and custom tool `renderCall`/`renderResult` functions.

The runtime registration maps prove some metadata/closures are retained, but there is no Rust-facing method to enumerate or execute shortcuts/message renderers comparable to tools/commands. `e-agent-node-runtime/src/lib.rs:14-19` exports tools and generic hostcalls only.

## Extension UI bridge seam

The bridge should be owned outside core and separate compatibility translation from native components:

```text
Pi TS extension
  -> e-agent-node-runtime HostcallRequest (Pi op/payload/raw input/ANSI)
  -> e-agent-pi-compat PiUiAdapter (translation, validation, capability negotiation)
  -> versioned renderer-neutral UiRequest / UiContribution broker
  -> selected TUI runner/component host
  -> correlated UiReply
  -> PiJsRuntime HostcallOutcome
```

Key ownership rules:

- `e-agent-node-runtime` keeps JS closure execution and request correlation; it must not depend on Ratatui.
- `e-agent-pi-compat` owns Pi names, payload translation, unsupported behavior, version/capability reporting, ANSI normalization, raw terminal key encoding, and failure isolation.
- renderer-neutral TUI code owns focus, component slots, overlays, UI state, and normalized input.
- Ratatui/Crossterm owns terminal-specific rendering, raw mode, screen selection, capabilities, and event decoding.
- CLI wires the selected TUI and Pi UI endpoint. Core remains unaware.

A broker is justified because modal requests are async and bidirectional, fire-and-forget contributions are persistent/replaceable by extension key, renderer tests need a fake endpoint, and a second frontend can implement the same protocol. `AgentEvent` and `HostAction` are not enough: `HostAction` currently only represents persistence/queue effects (`e-agent-core/src/tool/extension.rs:9-24`), and core drains it synchronously after extension observations (`e-agent-core/src/session/mod.rs:208-260`). UI dialogs can remain open while session events continue and require replies from the focused frontend.

## Compatibility and concurrency risks

### Runtime topology and deadlocks

- `PiCompat` runs one current-thread Tokio runtime plus `LocalSet` on a dedicated OS thread, processing `JsRuntimeCommand` serially (`e-agent-pi-compat/src/lib.rs:94-132`, `:134-286`). A modal extension handler can block that command loop awaiting a TUI reply. This is acceptable only if the TUI broker is serviced independently and does not await another Pi runtime command to render/respond.
- The TUI and session actor run on the CLI thread's `LocalSet` (`e-agent-core/src/session/handle.rs:116-135`; `e-agent-cli/src/main.rs:104-107`). Use channels/oneshots across the Pi thread boundary; never pass Ratatui state or non-`Send` component objects into the Pi thread.
- `JsRuntimeCommand` uses an unbounded channel (`e-agent-pi-compat/src/lib.rs:129-136`). Fire-and-forget updates and 60 Hz custom polling can grow queues if the UI stalls. The bridge needs coalescing/latest-value semantics for status/widget/frame invalidations and bounded outstanding modal/poll requests, rather than forwarding every render tick blindly.
- The existing custom shim polls every 16 ms and renders up to 30 fps (`e-agent-node-runtime/src/runtime.rs:21535-21569`). Combined with TUI's current 50 ms dirty tick (`e-agent-tui/src/lib.rs:247-275`), naïve forwarding creates redundant traffic and latency. Let the native render scheduler invalidate/coalesce, and return current width/input from the broker.

### Session/event consistency

- Broadcast lag is terminal for the current projection (`e-agent-tui/src/lib.rs:261-265`), while persistent extension widgets/status may outlive a lost event. On frontend failure/replace, clear or replay contribution state deterministically.
- `SessionAttachment` is single-consumer by value and consumes `Session`; renderer replacement should happen behind one stable runner/application instance, not by re-attaching the session.
- `run()` always aborts then closes on loop exit (`e-agent-tui/src/lib.rs:279-281`). A renderer swap must not trigger session shutdown. Distinguish renderer/component replacement from application exit.

### Focus, input, and IME

- Current input policy sends `Esc` straight to application exit and Ctrl-C to abort/exit before any component can consume it (`e-agent-tui/src/lib.rs:309-321`). Overlays, custom editors, selectors, and IME require focused-component dispatch first, followed by app keybindings.
- Current cursor position is computed from editor chars/CJK width and set directly in the Ratatui frame (`e-agent-tui/src/lib.rs:377-388`, `:726-740`). A replaceable component contract must return cursor visibility/position so the terminal driver can place the IME cursor; extension compatibility must not expose Ratatui `Position`.
- Crossterm resize/paste/focus events are currently ignored because only mouse and key branches are handled (`e-agent-tui/src/lib.rs:284-305`). Resize redraw may happen incidentally through terminal behavior, but extension custom UI width polling needs explicit capability/state updates.

### Lifecycle and cleanup

- `TerminalGuard` restores raw mode, mouse capture, and alternate screen on normal return and unwind (`e-agent-tui/src/lib.rs:742-755`), but setup is not transactional: if raw mode succeeds and `execute!(EnterAlternateScreen...)` fails, no guard exists to restore raw mode. The terminal driver should own staged rollback.
- Main-screen mode is impossible in the current unconditional `EnterAlternateScreen` path (`:742-747`). It belongs in terminal-driver configuration/capabilities, not the reducer.
- Panic/signal behavior is not covered by existing tests; retain RAII and add minimal real-terminal smoke rather than embedding cleanup in every renderer.

### Compatibility fidelity and isolation

- The current theme shim destroys ANSI/style semantics (`e-agent-node-runtime/src/runtime.rs:21277-21286`). The adapter must parse/translate the pinned documented subset or explicitly report degradation.
- Custom UI currently converts JS component output to arrays of strings and input to raw strings (`:21478-21532`). This is an intentionally isolated compatibility representation; do not force every native Rust component to use it.
- Extension identity is present on every hostcall (`runtime.rs:194-207`) and must be retained for slot ownership, cleanup, diagnostics, rate limits, and crash isolation.
- A failing extension renderer/component must remove only that extension's contribution and resolve/reject its request; it must not poison the session or terminal.
- `notify` is fire-and-forget in JS (`runtime.rs:21335-21345`). Failures can only be diagnostics/capability reports, not synchronous extension exceptions. Modal operations must resolve with documented cancel/default values on unsupported frontend, shutdown, timeout, or extension unload.

## Minimal staged migration

### Stage 0 — Lock evidence and protect the dirty worktree

- Add no production changes yet.
- Preserve current TUI snapshot/reducer/render tests and core attachment/event tests as migration gates.
- Record `e-agent-tool-ptc/src/ptc/mod.rs` as externally modified and leave it untouched.
- Add representative unchanged TypeScript fixtures for notify/status/widget, select/confirm/input/editor, custom overlay, shortcut, custom editor, and tool/message rendering. Existing runtime custom-width test (`runtime.rs:27942-28075`) is a starting fixture, not sufficient acceptance.

**Rollback:** no behavior change.

### Stage 1 — Extract renderer-neutral application state and effects inside `e-agent-tui`

- Move current projection/reducer and input outcomes behind plain Rust data types; retain `e_agent_tui::run(SessionAttachment)` as a wrapper.
- Represent input as normalized key/mouse/paste/resize/focus events and reducer outputs as effects (`Prompt`, `Abort`, `Close`, UI reply), keeping async calls outside reducer.
- Keep current Ratatui rendering behavior byte/fixture compatible.

**Gate:** existing tests pass plus deterministic reducer/effect tests without terminal.  
**Rollback:** wrapper continues to call old internal path until extraction is complete.

### Stage 2 — Introduce terminal/component/render scheduling seam

- Add only interfaces justified by a fake/test implementation and the Ratatui implementation: terminal lifecycle/capabilities, renderer/component host, focus/cursor, invalidation.
- Move Crossterm lifecycle/input decoding and Ratatui rendering behind the reference implementation.
- Add alternate/main-screen config and transactional setup rollback.
- CLI still calls the compatibility wrapper.

**Gate:** Ratatui `TestBackend` buffers, fake terminal lifecycle, focus/cursor/IME contract, and a trivial second/fake renderer consuming the same state.  
**Rollback:** select Ratatui implementation unconditionally.

### Stage 3 — Add versioned native extension UI broker and capability model

- Define renderer-neutral requests/replies/contributions outside core: dialogs, notifications, keyed status/widgets/header/footer/editor operations, overlays, focus handles, theme invalidation, terminal input subscription, clipboard text, and custom tool/message render requests.
- Broker owns extension-scoped cleanup, correlation, cancellation, bounded/coalesced updates, and unsupported replies.
- TUI runner services broker and session events concurrently.

**Gate:** fake broker tests for modal cancellation, contribution replacement/cleanup, backpressure/coalescing, frontend shutdown, and renderer replacement.  
**Rollback:** broker reports all optional capabilities unavailable; native TUI behavior remains unchanged.

### Stage 4 — Wire `e-agent-pi-compat` to the broker

- Pass frontend mode/capabilities into `PiCompat` at CLI composition time instead of hard-coded print/false (`pi-compat:478-485`).
- Translate `HostcallRequest::Ui` to broker operations and complete `HostcallOutcome` replies; stop acknowledging/dropping payloads (`:1056-1064`).
- Keep Pi/raw-input/ANSI handling entirely in this adapter.
- Update the capability matrix from one unversioned `custom-ui` flag to a pinned, versioned per-operation report.

**Gate:** unchanged TypeScript fixtures run in both headless and interactive fake-UI modes; headless calls return documented defaults and never hang.  
**Rollback:** CLI omits broker endpoint, yielding explicit headless capabilities.

### Stage 5 — Implement persistent contributions and standard dialogs in Ratatui

- Add notifications, status/working indicators, widgets, header/footer slots, replaceable editor, select/confirm/input/editor dialogs, theme invalidation, and focus/IME cursor.
- Route component input before global Esc/Ctrl-C policy.
- Preserve transcript/streaming/tool/multiline/scroll behavior.

**Gate:** deterministic buffer tests at narrow/normal/wide widths, CJK/incomplete Markdown, overlay focus, theme cache invalidation, paste/resize/mouse, and session fatal/abort.  
**Rollback:** disable individual capabilities in negotiation, not silent no-op.

### Stage 6 — Complete pinned Pi compatibility adapter

- Extend node-runtime Rust accessors/commands to enumerate and invoke retained shortcuts/message renderers/tool renderer closures.
- Implement custom component frame/input translation, overlay handles/focus, editor replacement, keybindings, terminal input subscriptions, clipboard text, themes, and custom call/result rendering through the isolated adapter.
- Do not expose Pi component types to native Rust components.

**Gate:** representative real extensions, including a Doom-like overlay fixture, execute substantially unchanged; unsupported edges are explicit and versioned.  
**Rollback:** capability-by-capability downgrade while standard native components remain available.

### Stage 7 — Expose compile-time frontend selection in CLI

- CLI constructs one selected frontend runner (Ratatui reference or second implementation/fake) and passes the same `SessionAttachment` and optional Pi UI endpoint.
- Keep `e_agent_tui::run(SessionAttachment)` during deprecation/migration.
- Demonstrate replacement without edits to `e-agent-core` or duplicated orchestration.

**Gate:** compile/contract test with two runner implementations; each receives identical snapshot/event/effect sequences.  
**Rollback:** select Ratatui runner by default/build feature.

## Recommended design constraints

- Keep `SessionAttachment`, `SessionClient`, `AgentEvent`, messages, and `SessionStatus` as the session-facing port; evolve only for demonstrated missing commands/resync.
- Do not add UI variants to `AgentEvent` or UI actions to core's `HostAction` merely to avoid a broker.
- Keep a stable application/TUI host while swapping active renderer/components; never consume a second attachment to replace rendering.
- Treat existing `run(SessionAttachment)` as a compatibility facade, not the long-term composition boundary.
- Capability negotiation must be versioned and operation-specific; unsupported calls must resolve/reject deterministically rather than no-op or hang.
- Coalesce keyed ephemeral state and render invalidations; serialize modal focus; preserve extension ID on every request.
- A second implementation can be selected solely in `e-agent-cli`; `e-agent-core`, providers, store, tools, and orchestration remain unchanged.
