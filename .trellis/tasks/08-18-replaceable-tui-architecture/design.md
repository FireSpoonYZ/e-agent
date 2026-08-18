# Replaceable TUI Architecture Design

## 1. Status and scope

This document defines the review target for a replaceable interactive terminal frontend. It is planning only. It does not change agent orchestration, persistence semantics, or the extension ABI.

The pinned compatibility target is the public, documented pi coding agent **0.84.2** UI surface. “Compatible” means representative existing TypeScript extensions using that surface run substantially unchanged. It does not mean a dynamic Rust plugin ABI, undocumented pi internals, or terminal image protocols.

The design preserves these existing facts:

- `e-agent-core` remains the authority for session state and exposes `SessionAttachment`, `SessionClient`, ordered `AgentEvent`s, restored messages, and `SessionStatus`.
- A frontend projects those facts and sends commands; it never orchestrates the agent.
- `e_agent_tui::run(SessionAttachment)` remains a migration facade.
- `e-agent-tool-ptc/src/ptc/mod.rs` is unrelated dirty work and must not be touched.

## 2. Design decisions

1. **Keep core unchanged.** UI requests, dialogs, decorations, and component registrations do not become `AgentEvent` or core `HostAction` variants.
2. **Use one stable interactive runner per attachment.** Renderer replacement happens inside that runner and never reattaches or restarts the session.
3. **Separate stable runner from replaceable renderer.** The runner owns session adaptation, reducer execution, effects, broker service, scheduling, and shutdown. A renderer owns layout and terminal-facing output; the runner owns portable component instances and focus state so they survive replacement.
4. **Normalize native input.** Crossterm events and pi raw ANSI strings are translated at adapters; reducers and native components receive semantic events.
5. **Keep pi compatibility isolated.** TypeScript callbacks, promises, AbortSignal behavior, ANSI/OSC strings, pi cursor markers, raw input, and pi theme/key names live in `e-agent-pi-compat`/`e-agent-node-runtime`, not native component contracts.
6. **Negotiate capabilities explicitly.** Unsupported or degraded operations return deterministic results and diagnostics; they never silently succeed or hang.
7. **Prefer concrete data over traits.** Traits exist only at replacement, test, or cross-runtime boundaries. State, events, effects, layout descriptions, themes, and protocol messages are enums/structs.
8. **Keep Ratatui/Crossterm as the reference implementation.** No blocking limitation was found.

## 3. Minimal crate and module architecture

The final dependency shape is:

```text
e-agent-cli (composition root)
  ├── e-agent-core
  ├── e-agent-tui                 # legacy facade
  ├── e-agent-tui-api             # renderer-neutral runner + UI protocol
  ├── e-agent-tui-ratatui         # reference renderer/terminal/components
  └── e-agent-pi-compat
        ├── e-agent-node-runtime
        └── e-agent-tui-api        # protocol types/client only

e-agent-tui -> e-agent-tui-api + e-agent-tui-ratatui
e-agent-tui-api -> e-agent-core
e-agent-tui-ratatui -> e-agent-tui-api + ratatui + crossterm
e-agent-core -> no TUI, terminal, Ratatui, Crossterm, pi, ANSI, or JS dependency
```

A separate `e-agent-tui-ratatui` crate is the intended final boundary. Cargo cannot preserve `e_agent_tui::run` from a neutral crate that the Ratatui crate also depends on: that would be a dependency cycle. Therefore `e-agent-tui` becomes a thin legacy facade, while neutral contracts move to `e-agent-tui-api`.

### 3.1 `e-agent-tui-api` modules

```text
attachment.rs   snapshot/event adaptation and projection inputs
state.rs        AppState, transcript/tool/editor/UI contribution projections
reducer.rs      pure state transitions and Effect production
input.rs        normalized input, command IDs, keybinding resolution data
runner.rs       select loop, effect execution, scheduling, cancellation, shutdown
render.rs       renderer contract, frame model, invalidation reasons
component.rs    native view/input/focus/cursor contracts
ui_protocol.rs  versioned broker requests/replies/contributions/capabilities
broker.rs       correlation, extension ownership, cleanup, backpressure
```

No module imports Ratatui, Crossterm, JavaScript runtime types, ANSI parsers, or pi declarations.

### 3.2 `e-agent-tui-ratatui` modules

```text
terminal.rs       transactional Crossterm lifecycle and capability detection
input.rs          Crossterm -> normalized input decoding
renderer.rs       Ratatui frame/buffer implementation and render modes
layout.rs         transcript/editor/slot/overlay layout
markdown.rs       pulldown-cmark conversion, including incomplete streams
components/       transcript, tool, editor, dialogs, selectors, status, slots
compat_ansi.rs    optional ANSI line painter used only by pi compatibility surfaces
```

### 3.3 Compatibility ownership

- `e-agent-node-runtime` retains JS closures and executes JS callbacks on its dedicated runtime thread.
- `e-agent-pi-compat` translates the pinned pi 0.84.2 API to/from `ui_protocol`, maps semantic input back to pi raw sequences when required, parses compatibility component output, and isolates callback failures.
- `e-agent-cli` creates the broker pair before constructing the extension host/session and selected frontend.

This avoids a new general “extension UI crate”: the neutral protocol already belongs with the frontend runtime, and only pi needs the compatibility translation.

## 4. Ownership and dependency rules

| Owner | Owns | Must not own |
|---|---|---|
| `e-agent-core` | authoritative session lifecycle, messages, tool execution, persistence, event order | UI projection, focus, dialogs, terminal state |
| interactive runner | one `SessionAttachment`, projected `AppState`, effect execution, broker service, render scheduling, shutdown | raw terminal mode, ANSI decoding, JS callback execution |
| renderer | native component tree, layout, focus/overlay presentation, frame output | session commands, extension runtime, agent lifecycle |
| terminal driver | raw mode, screen mode, keyboard/mouse/paste negotiation, cursor/title/progress, restoration | app policy, reducer state |
| UI broker | request correlation, extension ownership, cancellation, bounded/coalesced delivery, capability snapshot | pi names, ANSI, JS values |
| pi adapter | exact pi 0.84.2 semantics, JS callbacks/promises, raw input/ANSI translation, fallback behavior | Ratatui types, session orchestration |
| CLI | implementation selection and wiring | duplicated event loop or agent logic |

`SessionAttachment` is consumed exactly once. Renderer switches and component replacement are presentation operations and cannot invoke `abort`, `close`, or a second `attach`.

## 5. Public contracts

Signatures are illustrative; exact derives and error types may be refined without changing ownership.

### 5.1 Stable frontend runner and replaceable renderer

```rust
pub struct InteractiveOptions {
    pub screen_mode: ScreenMode,
    pub ui: Option<UiBrokerServer>,
}

pub enum ScreenMode { Main, Alternate }

#[async_trait::async_trait(?Send)]
pub trait InteractiveFrontend {
    async fn run(
        self: Box<Self>,
        attachment: SessionAttachment,
        options: InteractiveOptions,
    ) -> Result<ExitReason, FrontendError>;
}
```

`InteractiveFrontend` is the **CLI replacement seam**. It is justified by two linked implementations in contract tests (Ratatui and a fake/second frontend) and allows compile-time selection without core edits.

The reference frontend contains a stable runner with `renderer: Box<dyn Renderer>` and a CLI-provided renderer factory; `InteractiveRunner<R>` cannot switch to a different concrete renderer type:

```rust
pub trait Renderer {
    fn capabilities(&self) -> &UiCapabilities;
    fn mount(&mut self, snapshot: &RenderSnapshot, components: &mut ComponentRegistry) -> Result<(), RenderError>;
    fn render(&mut self, snapshot: &RenderSnapshot, components: &mut ComponentRegistry, damage: Damage) -> Result<(), RenderError>;
    fn handle_input(&mut self, event: &InputEvent, components: &mut ComponentRegistry) -> RendererInput;
    fn suspend(&mut self) -> Result<RendererPrivateState, RenderError>;
    fn resume(&mut self, state: Option<RendererPrivateState>) -> Result<(), RenderError>;
    fn shutdown(&mut self, preserve_screen: bool) -> Result<(), RenderError>;
}

pub enum RendererInput {
    Consumed(Vec<Effect>),
    Unhandled,
}
```

`Renderer` exists because Ratatui, a deterministic fake, and a second implementation consume the same snapshots, and because main/alternate implementations can be exchanged without recreating the runner. Portable focus/component/viewport/contribution state is runner-owned and present in `RenderSnapshot`; `RendererPrivateState` is optional and accepted only by a compatible renderer family.

A switch is rejected during an incompatible modal transition. Terminal ownership prevents a perfectly atomic swap, so rollback is best-effort:

1. pause scheduling and preflight the factory without acquiring terminal ownership;
2. suspend and stop the old renderer, retaining all portable state in the runner;
3. start/resume the new renderer, invalidate, render, publish capabilities, and rebind input;
4. on failure, restart the old renderer if possible; otherwise restore the terminal, settle pending requests, and exit cleanly.

No stable proxy trait is exposed to native components. Stable identity is represented by runner-owned `ComponentId`/`OverlayId` and broker handles.

### 5.2 Session adaptation and pure reducer

```rust
pub struct AppState {
    pub session: SessionProjection,
    pub interaction: InteractionState,
    pub ui: UiProjection,
}

pub enum AppEvent {
    Session(AgentEvent),
    Input(InputEvent),
    Ui(UiEnvelope),
    Timer(TimerId),
    CapabilitiesChanged(UiCapabilities),
    RendererFailed(RenderFailure),
    Shutdown(ShutdownCause),
}

pub fn reduce(state: &mut AppState, event: AppEvent) -> Vec<Effect>;

pub enum Effect {
    Session(SessionCommand),
    UiReply { request: RequestId, reply: UiReply },
    Render(Damage),
    SetTimer { id: TimerId, after: Duration },
    CancelTimer(TimerId),
    Clipboard(ClipboardEffect),
    SwitchRenderer(ScreenMode),
    Exit(ExitReason),
    Diagnostic(UiDiagnostic),
}

pub enum SessionCommand {
    Prompt(String),
    Steer(String),
    FollowUp(String),
    Abort,
    Close,
}
```

The reducer is a function, not a trait: replacement is unnecessary. It deterministically projects restored/live session facts, input, UI contributions, timers, and capability changes. Async work occurs only when the runner executes effects.

The session adapter initializes from attachment metadata/history/status and then applies events in receiver order. `MessageEnd` remains authoritative. Broadcast lag is initially a visible fatal adapter error because no resnapshot command exists; the design does not pretend recovery is possible.

### 5.3 Normalized input and command dispatch

```rust
pub enum InputEvent {
    Key(KeyEvent),
    Text(String),
    Paste(String),
    Mouse(MouseEvent),
    Resize { columns: u16, rows: u16 },
    FocusGained,
    FocusLost,
}

pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: Modifiers,
    pub kind: KeyKind, // Press, Repeat, Release
}

pub enum InputResult {
    Consumed(Vec<Effect>),
    Command(CommandId),
    Bubble,
}

pub struct CommandId(pub String); // e.g. "app.interrupt", "input.submit"
```

Dispatch order is:

1. raw pi terminal-input middleware, only when enabled, may observe/consume/transform bytes before native decoding;
2. focused visible capturing overlay/component;
3. active editor or selector;
4. context keymap to namespaced command;
5. application fallback.

Thus `Esc` and Ctrl-C are not unconditionally intercepted before overlays/editors. Key maps are data resolved by a concrete `KeybindingMap`, not a trait. Conflicts are reported. Unsupported release/super/modified-key semantics are reflected in capabilities.

Paste is text, never literal bracket markers in native state. The pi adapter may synthesize bracketed-paste raw input for a pi editor component.

### 5.4 Native component, focus, and cursor contracts

```rust
pub struct SemanticFrame { pub size: Size, pub cells: Vec<Cell> }

pub struct RenderContext<'a> {
    pub frame: &'a mut SemanticFrame,
    pub clip: Rect,
    pub theme: &'a Theme,
    cursor: &'a mut Option<CursorAnchor>,
}

pub struct InputContext<'a> {
    pub focused: bool,
    pub commands: &'a KeybindingMap,
}

pub trait Component {
    fn id(&self) -> ComponentId;
    fn render(&mut self, ctx: &mut RenderContext<'_>, area: Rect);
    fn input(&mut self, event: &InputEvent, ctx: &mut InputContext<'_>) -> InputResult;
    fn focus_changed(&mut self, focused: bool) {}
    fn invalidate(&mut self, reason: InvalidationReason) {}
}

pub struct CursorAnchor {
    pub component: ComponentId,
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub ime: bool,
}
```

`Component` is justified by replaceable editor/header/footer/widgets/overlays, deterministic component tests, and native reuse. It does not mimic pi's ANSI `render(width): string[]`. Renderer-neutral `Rect`, `Size`, `Cell`, `Style`, clipping, and grapheme/display-width rules define deterministic output; renderers translate the completed semantic frame to their backend.

Focus is centrally owned:

```rust
pub struct FocusManager { /* stack and previous-target history */ }

impl FocusManager {
    pub fn focus(&mut self, id: ComponentId) -> Result<(), FocusError>;
    pub fn unfocus(&mut self, id: ComponentId, target: Option<ComponentId>);
    pub fn remove(&mut self, id: ComponentId);
    pub fn focused(&self) -> Option<ComponentId>;
}
```

Only mounted, visible, capturing components can own focus. Hiding/removing one restores the next visible capturing overlay or previous target. Focus also determines overlay z-order. A component reports a semantic `CursorAnchor`; the renderer converts it to terminal coordinates and controls hardware cursor visibility for IME. Pi's `\x1b_pi:c\x07` marker is parsed only by the pi adapter.

### 5.5 Overlays and slots

```rust
pub struct OverlaySpec {
    pub placement: OverlayPlacement,
    pub width: Option<Size>,
    pub max_height: Option<Size>,
    pub margin: Insets,
    pub non_capturing: bool,
    pub responsive: Option<ResponsiveRule>,
}

pub struct OverlayHandle {
    pub id: OverlayId,
    broker: UiBrokerClient,
}

impl OverlayHandle {
    pub async fn hide(&self);                 // permanent
    pub async fn set_hidden(&self, hidden: bool);
    pub async fn focus(&self);
    pub async fn unfocus(&self, target: Option<ComponentId>);
}

pub enum Slot {
    Header,
    AboveEditor,
    Editor,
    BelowEditor,
    Footer,
}
```

The overlay data model is concrete. A handle exists because asynchronous extension code must manipulate a mounted overlay safely without direct renderer access. Handles are generation-checked; calls after disposal return `StaleHandle` and cannot affect a reused ID.

Header preserves pi semantics as startup/transcript content rather than fixed viewport chrome. Footer, widgets, editor, status, and working indicators are persistent slots. Widgets/statuses are keyed by `(ExtensionId, key)` and replacement disposes prior content.

### 5.6 Theme and invalidation

```rust
pub struct Theme {
    pub generation: u64,
    pub tokens: BTreeMap<ThemeToken, Style>,
    pub color_depth: ColorDepth,
}

pub enum InvalidationReason {
    State,
    Resize,
    Theme { old: u64, new: u64 },
    Capabilities,
    Full,
}

pub enum Damage { None, Components(BTreeSet<ComponentId>), Full }
```

Theme policy is semantic and renderer-neutral. Theme change increments generation, invokes component invalidation, clears Markdown/ANSI style caches, and schedules one coalesced full render. Native components use semantic tokens. The pi adapter supplies pinned pi token names and ANSI style functions and rebuilds cached compatibility frames on generation changes.

No `RenderScheduler` trait is needed. The runner owns a concrete dirty set and coalescing timer. `request_render` marks damage; urgent cursor/focus transitions may force an immediate render. At most one ordinary render is queued per scheduler interval.

## 6. Terminal lifecycle and capability model

### 6.1 Terminal driver

```rust
pub trait TerminalDriver {
    fn start(&mut self, config: TerminalConfig) -> Result<UiCapabilities, TerminalError>;
    fn next_event(&mut self) -> Result<InputEvent, TerminalError>;
    fn present(&mut self, frame: &Frame) -> Result<(), TerminalError>;
    fn set_cursor(&mut self, cursor: Option<CursorAnchor>) -> Result<(), TerminalError>;
    fn set_title(&mut self, title: &str) -> Result<(), TerminalError>;
    fn drain_input(&mut self, max: Duration, idle: Duration) -> Result<(), TerminalError>;
    fn stop(&mut self, preserve_screen: bool) -> Result<(), TerminalError>;
}
```

This trait is justified by Crossterm and a fake lifecycle driver, transactional setup tests, and main/alternate implementations. It is private to the concrete renderer crate unless a second renderer shares it.

Setup is transactional. Each successful mutation registers an inverse action: raw mode, VT input, bracketed paste, mouse capture, keyboard protocol, alternate screen, cursor hiding. On any later setup error, completed steps roll back in reverse order. `stop` is idempotent.

An RAII guard owns best-effort restoration for normal return and unwind. Signal shutdown coordinates extension/session cleanup before terminal writes where a dead terminal may return EIO. A panic hook invokes idempotent restoration and then delegates to the previous hook. Supported signal handlers, normal abort/fatal exit, and dead-output emergency paths are tested separately.

### 6.2 Versioned capabilities

```rust
pub struct UiProtocolVersion { pub major: u16, pub minor: u16 }

pub struct UiCapabilities {
    pub protocol: UiProtocolVersion,
    pub frontend: FrontendKind,
    pub screen_modes: BTreeSet<ScreenMode>,
    pub dialogs: DialogCapabilities,
    pub components: ComponentCapabilities,
    pub input: InputCapabilities,
    pub clipboard: ClipboardCapabilities,
    pub terminal: TerminalCapabilities,
    pub pi_compat: Option<PiCompatibility>,
}

pub struct PiCompatibility {
    pub target: String, // exactly "0.84.2"
    pub operations: BTreeMap<PiOperation, SupportLevel>,
}

pub enum SupportLevel {
    Native,
    Adapted,
    Degraded { reason: String },
    Unsupported { reason: String },
}
```

Capability snapshots are immutable values with change notifications after renderer/mode/terminal changes. Operations validate the protocol major version. This is a typed in-process channel, so there are no unknown serialized fields to ignore; additive minor fields use construction defaults and unknown typed operations receive `UnsupportedOperation`. The adapter exposes exact per-operation support rather than one `hasUI` flag, while still reporting `ctx.mode = "tui"` and `ctx.hasUI = true` when dialog/protocol UI is attached.

## 7. Versioned UI broker

### 7.1 Contract

```rust
pub struct UiEnvelope {
    pub version: UiProtocolVersion,
    pub extension: ExtensionId,
    pub request: RequestId,
    pub deadline: Option<Instant>,
    pub operation: UiOperation,
}

pub enum UiOperation {
    Dialog(DialogRequest),
    Notify(Notification),
    Contribution(ContributionOp),
    Editor(EditorOp),
    Overlay(OverlayOp),
    Theme(ThemeOp),
    Keybindings(KeybindingOp),
    TerminalInput(TerminalInputOp),
    Clipboard(ClipboardOp),
    Render(RenderRequest),
    Capabilities,
    Cancel { request: RequestId },
    UnloadExtension,
}

pub enum UiReply {
    Dialog(DialogResult),
    Value(UiValue),
    Handle(UiHandle),
    Capabilities(UiCapabilities),
    Ack,
    Unsupported { capability: String, reason: String },
    Cancelled,
    Failed { code: UiErrorCode, message: String },
}

#[derive(Clone)]
pub struct UiBrokerClient { /* bounded sender + correlation */ }
pub struct UiBrokerServer { /* runner receiver + reply channels */ }
```

The broker is justified by asynchronous request/reply dialogs across the dedicated JS thread, persistent extension-owned contributions, fake frontend tests, and reuse by a second frontend. It is not an agent event bus.

### 7.2 Ordering, backpressure, and cancellation

- Each extension's non-coalescible operations are observed in send order.
- Modal requests have a small bounded outstanding limit per extension and are serialized for focus. Excess requests receive `Busy`, not an unbounded queue.
- Keyed status, widget, title, working state, custom frame, and invalidation updates use latest-value coalescing by `(extension, operation, key)`.
- Notifications use a bounded FIFO; overflow drops oldest low-severity entries first and emits one diagnostic.
- Raw input delivery uses a bounded channel. A slow subscriber is disabled with a diagnostic rather than blocking terminal input.
- Render callbacks are demand-driven by invalidation/width/theme changes and frame-rate limited; the current 16 ms polling shim is not forwarded as unbounded work.
- Every request has a cancellation token tied to explicit cancellation, AbortSignal, timeout, extension unload, frontend shutdown, and component/handle disposal.
- Closing is idempotent. Late replies are discarded by request/generation identity.
- On unsupported/headless/shutdown paths, modal calls resolve to pi defaults (`undefined`/`false`) through the adapter; native callers receive typed unsupported/cancelled replies.

The runner `select!`s independently over session events, terminal input, broker messages, timers, render wakeups, and shutdown. It never waits synchronously for JS callback execution.

### 7.3 Failure isolation

- JS callback execution remains on the node runtime thread. It publishes immutable semantic frames/cursor metadata through channels; a runner-owned cached-frame component sends input/invalidation requests back. Renderer `render()` never invokes JS synchronously, and no non-`Send` component crosses threads.
- A failing custom component removes only its extension-owned contribution, restores focus/editor state, disposes best-effort, and rejects/resolves only its request.
- Tool/message/entry renderer failures fall back per row/item and emit diagnostics.
- Markdown transformer failure keeps output produced so far and continues the chain.
- `dispose()` errors are logged and ignored after ownership cleanup.
- Renderer failure is distinct from session fatal state. If recoverable, the runner may instantiate the configured fallback renderer; otherwise it restores the terminal and exits without manufacturing session events.
- Extension unload atomically cancels requests, removes owned slots/overlays/subscriptions/renderers, and invalidates once.

## 8. Pi 0.84.2 compatibility adapter

### 8.1 Translation boundary

```text
TypeScript extension
  -> node runtime retained closure / HostcallRequest
  -> PiUiAdapter (0.84.2)
  -> UiBrokerClient
  -> stable runner + selected renderer
  -> UiReply
  -> HostcallOutcome / JS Promise or callback
```

The CLI creates the broker before `PiCompat`, supplies the client and capability snapshot to the compatibility host, then passes the server to the selected frontend. Headless modes receive an explicit headless adapter that returns documented defaults.

### 8.2 Exact API behavior

The adapter supports unchanged public calls for:

- `select`, `confirm`, and `input`, including timeout/AbortSignal semantics; `editor(title, prefill)` has cancel semantics but no options argument in 0.84.2;
- `notify`, preserving non-blocking transcript-style interactive presentation and severity;
- keyed `setStatus`, working message/visibility/indicator, hidden-thinking label;
- keyed widgets and placement, header/footer factories and disposal, title;
- `custom<T>` replacement and overlay modes, async factories, `done`, overlay handles/focus;
- editor text get/set/paste, autocomplete wrapping, editor component get/set;
- theme discovery/get/set and tool expanded state;
- raw `onTerminalInput` subscribe/consume/transform/unsubscribe;
- registered shortcuts/keybindings;
- custom tool call/result, message, entry, and Markdown renderers.

No undocumented arbitrary clipboard read/write method is invented. Text clipboard compatibility consists of documented editor paste plus application selection/copy/paste behavior, capability-backed by platform or OSC 52. Image protocols remain unsupported and are reported as such.

### 8.3 ANSI component adapter

A pi component remains width-constrained. A JS-thread `PiComponentProducer` invokes callbacks and returns owned ANSI lines; the pi adapter parses replies into immutable semantic frames, and a runner-owned `PiCachedComponent` renders the latest frame without owning a JS closure. Width/theme/invalidation sends a bounded render request. The producer/adapter path:

1. invoke the JS callback with the current width/theme/keybindings/TUI proxy on the runtime thread and return owned lines;
2. validate line count in the pi adapter and display width with ANSI/OSC ignored;
3. append/reset SGR and OSC 8 state per line;
4. parse supported ANSI styles and hyperlinks into semantic cells;
5. strip and translate the pi cursor marker to `CursorAnchor`;
6. replace unsupported/control sequences with safe text or omit them and emit a degradation diagnostic;
7. clip overflow to the assigned area so an extension cannot corrupt adjacent UI.

ANSI/raw details never enter `Component`, `AppState`, or `Renderer` APIs. CJK width uses one shared display-width implementation at the adapter/render boundary.

### 8.4 Raw input and JS callbacks

The terminal driver may retain raw bytes alongside decoded native events only when `raw_terminal_input` is negotiated. The pi adapter applies listeners in registration order. Each can observe, consume, or transform data. Transformed bytes are decoded again before native dispatch; invalid sequences are bounded and diagnosed. Unsubscription and renderer switching rebind listeners without duplicate delivery.

For a pi-focused component, normalized events are encoded into the pinned pi key/Kitty representation. Release events are sent only when the component requests them and the terminal capability exists. Unsupported event kinds degrade to press semantics as reported by capabilities.

Callbacks are invoked on the JS runtime thread through bounded commands. Deadlines stop the frontend awaiting late callbacks and keep session/terminal processing responsive, but cannot preempt a synchronous infinite loop on the single JS runtime thread; that runtime may remain wedged. Tool render callback state, `lastComponent`, stable tool-call ID, partial/expanded/error flags, and self/default shell policy are retained per registered renderer slot. Fallback rendering is native and always available.

### 8.5 Stable TUI proxy for extensions

The adapter exposes a stable JS TUI proxy whose methods target broker-owned IDs and current capabilities. It does not expose a Rust renderer pointer. On main/alternate or concrete renderer replacement, proxy identity and component handles remain stable; the runner remounts state, publishes capabilities, invalidates, and rebinds terminal-input listeners.

## 9. Main-screen and alternate-screen modes

`ScreenMode` is construction policy, not reducer state.

- **Main** renders on the regular screen and preserves native terminal scrollback. Transcript capture/restoration and clear-on-shrink behavior belong to the renderer.
- **Alternate** owns the viewport and provides follow mode, scrolling, prompt jumps, search, scrollbar, mouse selection, edge auto-scroll, and link handling.

Both modes consume identical `RenderSnapshot`s and component/overlay state. Mode-specific capabilities are explicit. Switching is denied while an operation cannot be safely remounted; otherwise it uses the renderer suspension sequence and does not cancel the session. Main mode is the lower-risk rollback/default if alternate-screen-specific interaction fails; the product default may remain current alternate behavior during migration to preserve expectations.

## 10. End-to-end data flow

```text
SessionAttachment snapshot ─┐
AgentEvent receiver ─────────┤
Terminal/Crossterm input ────┤-> runner -> AppEvent -> pure reduce -> Effect[]
UI broker requests ──────────┤                              |
Timers/capability changes ───┘                              +-> SessionClient
                                                            +-> UiBroker reply
                                                            +-> clipboard/timer
                                                            +-> dirty set
                                                            +-> renderer snapshot

Pi extension -> node hostcall/callback -> PiUiAdapter -> broker -> same runner
```

Rendering reads an immutable `RenderSnapshot` derived from `AppState`; renderers cannot mutate session projection. Component-local edit/focus/overlay changes return input results/effects and become reducer events before the next snapshot.

Shutdown sequence:

1. stop accepting new broker/session input;
2. cancel pending modal/callback work and remove extension contributions;
3. on signal, dispose extension/runtime resources before risky terminal writes;
4. drain negotiated keyboard input where safe;
5. idempotently restore terminal modes/screens/cursor;
6. execute session abort/close only for actual application exit, never renderer replacement;
7. return `ExitReason` to CLI.

## 11. Capability matrix and acceptance evidence

“M1” means required in the first complete reference milestone. Later work may be staged internally, but M1 is not accepted until every required row passes.

| Pi/reference capability | Rust/native mapping | Disposition | Acceptance test |
|---|---|---|---|
| restored transcript; streaming assistant/thinking/tool | session projection + transcript/tool components | M1 | restored, partial, finalized, fatal fixtures |
| Markdown, incomplete streams, CJK width | native Markdown component | M1 | narrow/normal/wide buffers; tables; incomplete Markdown; CJK |
| multiline editor | native editor slot | M1 | newline/delete/navigation/history/paste tests |
| replaceable editor | editor component slot + adapter factory | M1 | unchanged modal editor wrapping prior factory; restore/dispose |
| focus and IME | focus manager + semantic cursor anchor | M1 | nested focus, CJK cursor, hardware cursor capability |
| keybindings/commands | normalized input + namespaced map | M1 | representative remap/conflict; overlay precedence |
| select/confirm/input/editor | typed modal requests/results | M1 | unchanged TS fixture; submit/cancel; timeout/AbortSignal for select/confirm/input only |
| custom non-overlay UI | temporary editor replacement | M1 | preserve text/focus, async rejection, idempotent done, disposal |
| overlays/options/handles | overlay stack/layout/focus IDs | M1 | pinned overlay QA: anchors, stacking, responsive hide/show, focus/unfocus |
| header/footer/widgets | named/keyed slots | M1 | replacement/removal/placement/disposal; footer data refresh |
| status/working indicators | keyed status/activity models | M1 | multiple keys, reset, hidden/static/animated frames |
| notifications | bounded notification model | M1 | severity/order/overflow and non-blocking call |
| themes | semantic theme + generation invalidation | M1 | theme switch rebuilds cached native and ANSI content |
| key release/raw input subscriptions | capability-gated adapter middleware | M1 | consume/transform/unsubscribe/rebind; release downgrade |
| editor text/paste/autocomplete | active editor commands/providers | M1 | multiline/large paste; provider delegation; no literal markers |
| text clipboard | renderer capability + platform/OSC 52 fallback | M1 baseline | copy/paste capability matrix and fullscreen selection smoke |
| custom tool call/result | renderer registry + per-row fallback | M1 | partial/expanded/state/lastComponent/throw/self-shell fixture |
| custom message/entry renderer | transcript renderer registry | M1 | persist/restore entry; model-context message; throw fallback |
| Markdown transformers | transform chain before native Markdown | M1 | chain/failure continuation/streaming/resize fixture |
| title | terminal effect | M1 | set/replace and unsupported terminal report |
| tool expanded state | UI projection + command | M1 | toggle and custom renderer context |
| main vs alternate screen | renderer factories over stable runner | M1 | mode switch preserves components/focus and does not close session |
| resize/follow/scroll | normalized resize + mode viewport policy | M1 | resize reflow; follow break/resume; wheel/keys |
| baseline mouse/links | normalized pointer + renderer hit map | M1 | selection, wheel, OSC 8 link smoke where supported |
| terminal capability reporting | versioned snapshot/support level | M1 | fake matrix; every unsupported op returns explicit result |
| renderer replacement | CLI factory + stable runner | M1 | fake second renderer receives same snapshot/effect sequence; no core edit |
| extension cleanup/failure isolation | broker ownership/generation | M1 | unload during modal, callback throw/hang, stale handle, fallback |
| images/terminal image protocols | none | rejected/out of scope | capability reports unsupported; no silent rendering |
| undocumented pi internals | none | rejected | fixture uses only pinned public declarations |
| advanced keyboard/clipboard protocols beyond need | capability extension point only | deferred | explicit degraded/unsupported report |

Overlay fixtures must additionally pin the 0.84.2 `custom()` close behavior that hides the topmost overlay when stacks exist. Representative unchanged TypeScript fixtures must be derived from the pinned examples for overlay QA, custom header/footer/widgets, modal editor, working indicator, autocomplete, tool rendering, message/entry rendering, Markdown transforms, theme switching, and terminal input. Rust mocks supplement but do not replace these fixtures.

## 12. Trait justification audit

| Trait | Why it exists | Concrete implementations/users |
|---|---|---|
| `InteractiveFrontend` | compile-time CLI replacement | Ratatui frontend; fake/second frontend contract test |
| `Renderer` | runtime main/alternate/concrete renderer replacement and deterministic rendering | Ratatui renderer; fake renderer |
| `Component` | replaceable native editor/slots/overlays and component tests | built-ins; native custom components; test component |
| `TerminalDriver` | transactional lifecycle/capability testing and terminal substitution | Crossterm driver; fake driver |

No traits are proposed for reducer, scheduler, theme, keybindings, focus manager, overlay store, protocol messages, or capability data. They are ordinary concrete values because no second behavior is required.

The broker client/server are channel-backed handles, not traits. This provides the required process/thread and fake-frontend seam without an interface hierarchy.

## 13. Migration, public stability, and rollback

### 13.1 Public API

- Keep `e_agent_tui::run(SessionAttachment)` in the `e-agent-tui` facade, which depends on `e-agent-tui-api` and `e-agent-tui-ratatui`; this avoids a Cargo cycle.
- Add the generic frontend API without changing `e-agent-core`.
- Mark the facade deprecated only after CLI selection and downstream callers have a full release cycle; no immediate removal.
- Pin UI protocol major version and pi target `0.84.2`. Additive protocol fields increment minor; breaking changes increment major and require explicit negotiation.

### 13.2 Rollout order

1. Freeze current session/reducer/render behavior as tests; protect unrelated dirty files.
2. Extract plain state/events/effects while the facade still follows the old path.
3. Introduce renderer/terminal seams and Ratatui deterministic backend; keep Ratatui selected unconditionally.
4. Add broker/capability model with all optional capabilities explicitly unavailable.
5. Wire PiCompat mode/hostcalls to a fake broker; headless defaults remain available.
6. Add native slots/dialogs/focus/theme/input behavior and enable capabilities individually.
7. Complete ANSI/raw-input/JS callback compatibility and unchanged TS fixtures.
8. Move concrete implementation to `e-agent-tui-ratatui` and expose CLI compile-time selection.
9. Enable renderer mode switching only after preservation tests pass.

The workspace remains buildable after every stage. Existing transcript, streaming, tool, Markdown, editor, scroll/follow, abort/fatal, CJK, and cleanup tests are gates at every extraction stage.

### 13.3 Rollback points

- Before broker wiring: facade selects the old/reference path.
- Broker present but UI incomplete: advertise unsupported capabilities; native TUI behavior is unchanged.
- Pi adapter regression: omit broker endpoint and run explicit headless/print compatibility; never acknowledge and drop requests.
- Individual contribution regression: disable only that operation in capabilities and return documented defaults/fallback rendering.
- alternate-screen regression: select main/reference mode without changing runner/core.
- new frontend regression: CLI selects Ratatui implementation by default/build configuration.

Rollback never requires reverting core, duplicating session orchestration, or consuming another `SessionAttachment`.

## 14. Verification strategy

### Pure/deterministic tests

- reducer: restored snapshot plus ordered events, streaming/finalization, tool lifecycle, fatal/shutdown, normalized inputs to effects;
- broker: request correlation, per-extension order, modal serialization, coalescing, overflow, timeout/cancel/unload/shutdown, stale handles;
- renderer: Ratatui `TestBackend`/buffer snapshots at narrow/normal/wide sizes, CJK, incomplete Markdown, themes, overlays, widgets, main/alternate layout;
- components: focus transitions, input bubbling, cursor anchors, editor replacement, selector/dialog results;
- replacement: two renderer/frontends receive identical snapshots and produce equivalent session effects; swap preserves focus/editor/overlay state;
- adapter: pinned TS fixtures for every matrix row, ANSI overflow/control sanitization, raw input transformation, callback throw/hang fallback.

### Minimal real-terminal smoke tests

Use a PTY where supported to verify staged setup rollback and restoration on normal exit, abort/fatal, signal, and panic/unwind; main/alternate enter/leave; cursor visibility; bracketed paste; resize; mouse; and slow-input drain. Platform-unavailable behavior is asserted through capability snapshots rather than skipped silently.

## 15. Rejected alternatives

### Put UI events in `AgentEvent`

Rejected because dialogs and contributions are ephemeral, bidirectional, frontend-specific, and extension-owned. It would make core depend on UI lifecycle and still would not solve request/reply correlation.

### Put UI effects in core `HostAction`

Rejected because `HostAction` is synchronously drained for persistence/queue effects. A modal UI may remain open while session events continue and belongs outside orchestration.

### Let each renderer attach to the session

Rejected because `SessionAttachment` is single-consumer, attach ordering matters, and renderer replacement must not restart/close the agent. One stable runner owns the attachment.

### Make Ratatui buffers or pi ANSI lines the universal component API

Rejected because either choice couples every implementation to one toolkit/compatibility format. Native components use semantic rendering; only the pi adapter handles ANSI lines.

### Dynamic libraries or external frontend processes

Rejected as out of scope. Ordinary Rust compile-time composition satisfies replacement without introducing ABI/versioning/IPC complexity.

### One trait per subsystem

Rejected. Reducer, scheduler, focus, theme, keybindings, and capability logic need deterministic data/functions, not speculative polymorphism. Only four interfaces have demonstrated replacement/test users.

### Unbounded broker channels or 60 Hz polling passthrough

Rejected because the node command channel and custom UI polling can outpace rendering. Bounded queues, keyed coalescing, demand-driven invalidation, and explicit overload behavior are required.

### Silent no-op compatibility

Rejected because current dropped UI payloads misreport support and can hang modal expectations. Every unsupported call returns a documented default or typed unsupported result and appears in the capability model.

### Clone undocumented pi internals and image protocols

Rejected. Compatibility is pinned to public 0.84.2 declarations/examples. Images and undocumented behavior add cost without acceptance value.

## 16. Review conclusions

This design creates one explicit composition seam at the CLI, one stable runner over the existing core attachment, and a small set of justified replacement/test interfaces. Ratatui/Crossterm and pi/ANSI/JavaScript concerns remain adapters at the edge. A second frontend can be linked and selected by changing only CLI construction; it consumes the same attachment/protocol and does not duplicate session orchestration or modify `e-agent-core`.
