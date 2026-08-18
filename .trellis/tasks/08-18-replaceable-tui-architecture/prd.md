# Design Replaceable TUI Architecture

## Goal

Design a complete Rust TUI abstraction for e-agent, informed by pi agent's TUI architecture and interaction model, so the concrete TUI implementation can be replaced without changing agent/session core behavior. Then plan a Rust reference implementation that provides the agreed pi-like interactive experience.

## Background and Confirmed Facts

- `e-agent-core` already exposes an event-driven UI boundary through `SessionAttachment`, `SessionClient`, `AgentEvent`, persisted messages, and `SessionStatus`; authoritative agent state is not assembled by the TUI.
- `e-agent-cli/src/main.rs` currently selects interactive mode by directly calling `e_agent_tui::run(session.attach())`, so the CLI is coupled to one concrete TUI crate and entry point.
- `e-agent-tui/src/lib.rs` currently combines all concerns in one module: terminal lifecycle, Crossterm event decoding, async event loop, application reducer/state, input editing, interaction policy, Ratatui layout/rendering, Markdown conversion, and tests.
- The existing Rust TUI already supports restored history, streamed assistant/thinking/tool state, Markdown, multiline editing, scrolling/follow mode, abort/fatal handling, CJK width, and terminal cleanup.
- The installed pi coding agent (`0.84.2`) separates its session/runtime from interactive mode, has a composition root for main-screen versus alternate-screen renderers, and uses a stable TUI reference while replacing the active renderer.
- Pi's reusable component contract is width-constrained rendering plus optional input, focus/IME behavior, invalidation, and lifecycle. Its interactive layer supports replaceable editor/footer/header, widgets, overlays, selectors, notifications, tool renderers, themes, keybindings, and render scheduling.
- Prior project decisions establish that frontends consume typed session events and invoke command methods; TUI/web/GUI must not own agent orchestration.

## Requirements

### R1 — Core Independence

- Agent/session/provider/tool/store behavior must not depend on Ratatui, Crossterm, terminal concepts, or a concrete TUI implementation.
- A TUI consumes session snapshots/events and sends commands only through stable session-facing ports.

### R2 — Fully Replaceable Implementation

- The concrete terminal driver, renderer/component toolkit, layout, styling, and interaction implementation must be replaceable behind an explicit composition boundary.
- Replacing the reference TUI must not require edits to `e-agent-core` or agent orchestration.
- The replacement boundary is ordinary Rust compile-time composition: implementations are linked and selected at the CLI composition root; no dynamic-library ABI or external runtime plugin loading is required.

### R3 — Explicit Responsibility Boundaries

The design must separate at least:

- session/event adaptation and UI-facing state projection
- application reducer and interaction effects
- input/key/mouse/paste/resize event representation
- render scheduling and invalidation
- view/component contracts and focus
- terminal lifecycle and capabilities
- theme/keybinding policy
- concrete Ratatui/Crossterm implementation

Boundaries must avoid speculative one-implementation interfaces: each abstraction must enable replacement, deterministic testing, or reuse by another frontend.

### R4 — Pi-Informed Capability Model

The design must evaluate and deliberately include, defer, or reject these pi-like capabilities:

- transcript and streaming assistant/thinking/tool rendering
- multiline editor and editor replacement
- focus and IME cursor positioning
- keybindings and command dispatch
- overlays/dialogs/selectors
- persistent widgets, header, footer, and status/working indicators
- notifications
- custom tool call/result rendering
- themes and cache invalidation
- main-screen versus alternate-screen operation
- resize, scrolling/follow mode, clipboard/paste, mouse, and terminal capability handling
- extension-provided UI contributions where compatible with the project's extension boundary

### R5 — Rust Reference Implementation

- Provide a Rust implementation of the agreed abstraction using the existing Ratatui/Crossterm stack unless research finds a blocking limitation.
- Preserve all currently working TUI behavior during migration.
- The first implementation milestone includes the complete replacement boundary plus transcript/streaming/tool views, multiline and replaceable editor, keybindings/commands, focus and IME support, overlays/dialogs/selectors, theme invalidation, status/notifications/widgets, header/footer slots, main/alternate screen modes, baseline paste/resize/scroll/mouse behavior, and extension-authored custom UI.
- The compatibility target is the complete public, documented pi `ctx.ui` surface for the pinned reference version, including select/confirm/input/editor, custom overlays and handles/focus, editor/header/footer/widgets, status/working indicators/notifications, themes/keybindings, terminal input subscriptions, documented editor paste and application text-copy behavior, and custom tool call/result rendering.
- Undocumented pi internals and terminal image protocols are not compatibility requirements. Deferred or platform-unavailable features must be reported through a versioned capability model rather than failing silently.
- The compatibility surface must be isolated in an adapter: pi/ANSI/raw-input details must not become requirements on every native Rust component or concrete renderer. The design must specify translation, capability negotiation, lifecycle, failure isolation, and unsupported-edge behavior.
- Existing pi TypeScript extensions using the documented UI APIs for the pinned reference version must run substantially unchanged; compatibility fixtures must exercise representative extensions rather than only Rust mocks.
- Add functionality in staged, independently verifiable increments rather than a monolithic rewrite.

### R6 — Reliability and Testability

- Reducer and interaction logic must be testable without a real terminal.
- Rendering must be testable with a deterministic backend/buffer.
- Terminal setup must be restored on normal exit, abort, fatal session state, signal handling, and panic/unwind paths supported by the platform.
- Streaming, narrow/normal/wide terminals, CJK width, incomplete Markdown, focus, overlays, resize, and renderer replacement require contract coverage appropriate to the final scope.

### R7 — Compatibility and Migration

- Existing `e_agent_tui::run(SessionAttachment)` behavior must have a clear migration path.
- Existing uncommitted work outside this task, including `e-agent-tool-ptc/src/ptc/mod.rs`, must not be overwritten.
- The design must identify public API stability, rollout order, and rollback points.

## Acceptance Criteria

- [ ] `design.md` defines the abstraction layers, ownership rules, public contracts, event/effect/data flow, lifecycle, replacement seam, and dependency direction.
- [ ] The design demonstrates how a second TUI implementation can be selected without changing `e-agent-core` or duplicating session orchestration.
- [ ] The design contains a pi capability matrix: reference behavior, Rust mapping, MVP/full-phase disposition, and acceptance test.
- [ ] Every proposed trait/interface has at least one concrete replacement/testing/reuse reason.
- [ ] `implement.md` provides ordered migration stages that preserve current functionality and keep the workspace buildable.
- [ ] The plan specifies deterministic reducer, renderer, component/focus, and replacement contract tests plus minimal real-terminal smoke tests.
- [ ] Representative unchanged pi 0.84.2 TypeScript fixtures cover every extension-facing first-milestone capability; unsupported images and platform limitations are explicit in versioned capabilities.
- [ ] The user reviews and approves the final PRD/design/implementation plan before implementation starts.

## Out of Scope Unless Explicitly Chosen

- Changing agent orchestration or persistence semantics.
- A stable cross-language or dynamic-library plugin ABI.
- Runtime loading of external TUI binaries or libraries.
- Rebuilding web/GUI frontends as part of this task.
- Blind one-to-one cloning of pi screens that do not support an agreed capability.
- Image protocols and advanced keyboard/clipboard protocols, except where later evidence shows they are required for extension UI compatibility.
