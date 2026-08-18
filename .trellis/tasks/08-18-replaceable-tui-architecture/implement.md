# Replaceable TUI Architecture — Implementation Plan

## 1. Execution rules

This plan implements the reviewed `prd.md` and `design.md` in small, buildable deliverables. Complete stages in order; do not begin a dependent stage until its review gate passes. A stage may use several commits, but no parent/child Trellis task tree is needed—the stages below are the independently verifiable units.

### Invariants for every stage

- `e-agent-core` remains unchanged unless implementation discovers a demonstrable missing session-facing contract and the design is reviewed again first. Never add Ratatui, Crossterm, terminal, ANSI, JavaScript, or UI broker types to core.
- One runner consumes one `SessionAttachment`; renderer changes never reattach, abort, or close the session.
- Keep the package/API `pub async fn e_agent_tui::run(SessionAttachment)` working. At Stage 7, move neutral contracts to `e-agent-tui-api` and retain `e-agent-tui` as a facade; do not create a dependency cycle.
- Ratatui/Crossterm behavior remains the default until the replacement and compatibility suites are green.
- Unsupported pi operations return typed/versioned support information and documented defaults; never acknowledge-and-drop or leave a promise pending.
- Add only the four reviewed interfaces: `InteractiveFrontend`, `Renderer`, `Component`, and the concrete-renderer-private `TerminalDriver`. Reducer, scheduler, focus, theme, keybindings, capabilities, and broker remain concrete data/functions.
- After each stage run formatting, the stage-specific tests, and at least `cargo check --workspace`. Keep warnings and ignored/skipped platform checks visible in the review record.

### Dirty-worktree protection

`e-agent-tool-ptc/src/ptc/mod.rs` contains unrelated user work. Before Stage 0, capture its patch fingerprint without modifying it:

```bash
PTC_PATCH_HASH="$(git diff -- e-agent-tool-ptc/src/ptc/mod.rs | git hash-object --stdin)"
git diff -- e-agent-tool-ptc/src/ptc/mod.rs > .trellis/tasks/08-18-replaceable-tui-architecture/research/ptc-preexisting.patch
```

The patch file is a safety record only; do not apply, edit, stage, or commit it. At every stage gate run:

```bash
test "$PTC_PATCH_HASH" = "$(git diff -- e-agent-tool-ptc/src/ptc/mod.rs | git hash-object --stdin)"
git status --short
git diff --check
```

If the hash changes, stop and ask the user whether they edited it concurrently; do not restore it automatically. Never use broad `git add -A`, `git checkout .`, `git restore .`, `git reset --hard`, `git clean`, or stash operations. Stage explicit paths only, always excluding `e-agent-tool-ptc/src/ptc/mod.rs`.

## 2. Stage 0 — Freeze baseline behavior and fixture provenance

**Depends on:** reviewed PRD/design/plan only.

### Changes

- Preserve the current tests and add missing behavior assertions without moving or splitting production code; structural extraction begins in Stage 1.
- Add a fixture provenance manifest for pi coding agent `0.84.2`, recording source paths and SHA-256 hashes.
- Copy representative public examples unchanged into `e-agent-pi-compat/tests/fixtures/pi-0.84.2/extensions/`:
  - `overlay-qa-tests.ts`
  - `custom-header.ts`
  - `custom-footer.ts`
  - `modal-editor.ts`
  - `widget-placement.ts`
  - `working-indicator.ts`
  - `github-issue-autocomplete.ts`
  - representative tool rendering example (`todo.ts` or the pinned documented tool-render example)
  - `message-renderer.ts`
  - `entry-renderer.ts`
  - the pinned Markdown transformer example
  - `mac-system-theme.ts`
  - a terminal-input subscription example from the pinned documentation/examples
- Do not alter fixture source to fit the host. Put any path/import adaptation in the test harness, and record examples that are documentation-derived rather than file-derived in the manifest.
- Record the current unsupported/headless pi assertions so later stages deliberately replace them rather than silently weakening them.

### Likely files

- `e-agent-tui/src/lib.rs` (tests only; production logic unchanged)
- `e-agent-pi-compat/tests/fixtures/pi-0.84.2/manifest.json`
- `e-agent-pi-compat/tests/fixtures/pi-0.84.2/extensions/*.ts`
- `e-agent-pi-compat/tests/pi_ui_fixtures.rs` (fixture integrity/load smoke only)

### Minimal validation

```bash
cargo test -p e-agent-core
cargo test -p e-agent-tui
cargo test -p e-agent-node-runtime pijs_custom_ui_width_updates_trigger_reflow
cargo test -p e-agent-pi-compat
cargo check --workspace
```

### Review gate and rollback

- Existing restored history, ordered streaming/finalization, thinking/tool display, Markdown/incomplete Markdown, multiline editor, scrolling/follow mode, abort/fatal, CJK width, and cleanup assertions all remain green.
- Fixture hashes match the pinned sources and fixtures are not rewritten by the harness.
- **Rollback:** remove only the new fixture copies/manifest/tests; runtime behavior is unchanged.

## 3. Stage 1 — Extract renderer-neutral state, input, reducer, and effects

**Depends on:** Stage 0.

### Changes

Split `e-agent-tui/src/lib.rs` while retaining the current event loop and renderer path:

- `attachment.rs`: initial snapshot conversion and ordered `AgentEvent` adaptation; preserve visible-fatal behavior on broadcast lag.
- `state.rs`: `AppState`, session projection, interaction state, editor/viewport values, and immutable `RenderSnapshot` construction.
- `input.rs`: normalized key/text/paste/mouse/resize/focus events, namespaced `CommandId`, and concrete keybinding data.
- `reducer.rs`: pure `reduce(&mut AppState, AppEvent) -> Vec<Effect>` and concrete session command/effect enums.
- `runner.rs`: execute effects against `SessionClient`; keep async work out of the reducer.
- `lib.rs`: exports plus the unchanged `run(SessionAttachment)` facade.

Crossterm decoding may remain in `lib.rs` temporarily, but only it may construct normalized native input. Preserve current key behavior during this extraction; focused-component precedence lands in Stage 5.

### Likely files

- `e-agent-tui/src/lib.rs`
- `e-agent-tui/src/attachment.rs`
- `e-agent-tui/src/state.rs`
- `e-agent-tui/src/input.rs`
- `e-agent-tui/src/reducer.rs`
- `e-agent-tui/src/runner.rs`
- `e-agent-tui/Cargo.toml` only if an already-used dependency must move between normal/dev sections

### Minimal tests

- Snapshot plus buffered ordered events produces the same projection as the old path.
- `MessageEnd` replaces streaming state authoritatively.
- Tool start/update/end, fatal, shutdown, and lag produce the same visible state/effects.
- Normalized submit/steer/follow-up/abort/close inputs produce session effects without a terminal.
- Existing editor and viewport tests remain unchanged and green.

```bash
cargo fmt --all -- --check
cargo test -p e-agent-tui
cargo test -p e-agent-core
cargo check --workspace
```

### Review gate and rollback

- No Ratatui/Crossterm/ANSI/JS types appear in `attachment.rs`, `state.rs`, `input.rs`, or `reducer.rs`.
- The facade follows the extracted reducer but produces no intentional visible behavior change.
- **Rollback:** point the facade back to the pre-extraction internal loop while correcting the extracted modules; do not alter core.

## 4. Stage 2 — Add renderer, component, focus, scheduling, and terminal seams

**Depends on:** Stage 1.

### Changes

Introduce the reviewed replacement/test boundaries while the concrete implementation still lives in `e-agent-tui`:

- `render.rs`: `Renderer`, `Damage`, invalidation reasons, a concrete semantic `Cell`/`Style`/`SemanticFrame` buffer with clipping/display-width rules, snapshot/private-renderer-state values, and a concrete dirty-set/coalescing scheduler. Runtime replacement uses `Box<dyn Renderer>` plus a factory, not `InteractiveRunner<R>`.
- `component.rs`: `Component`, `ComponentId`, semantic input result, `CursorAnchor`, and concrete `FocusManager`.
- `ratatui/renderer.rs`, `ratatui/layout.rs`, `ratatui/markdown.rs`, `ratatui/components/*`: move existing rendering without changing output.
- `ratatui/input.rs`: translate all relevant Crossterm events, including paste, resize, focus, mouse, and key press/repeat/release.
- `ratatui/terminal.rs`: renderer-private `TerminalDriver`, Crossterm implementation, fake driver, transactional start rollback, idempotent stop, and RAII restoration.
- Add `ScreenMode::{Main, Alternate}` as construction policy. Alternate remains the default at this gate; main mode can initially advertise only implemented capabilities.

The scheduler coalesces ordinary invalidations and allows immediate focus/cursor redraws. It is a concrete runner facility, not another trait.

### Likely files

- `e-agent-tui/src/render.rs`
- `e-agent-tui/src/component.rs`
- `e-agent-tui/src/runner.rs`
- `e-agent-tui/src/ratatui/{mod.rs,renderer.rs,layout.rs,markdown.rs,input.rs,terminal.rs}`
- `e-agent-tui/src/ratatui/components/{mod.rs,transcript.rs,tool.rs,editor.rs}`
- `e-agent-tui/src/lib.rs`
- `e-agent-tui/Cargo.toml`

### Minimal tests

- Ratatui `TestBackend` buffers at narrow/normal/wide widths, including CJK and incomplete Markdown, match Stage 0 behavior.
- A fake renderer consumes snapshots and records damage without a terminal.
- Component focus only targets mounted/visible/capturing IDs; hide/remove restores prior focus; cursor anchors survive CJK positions; fake and Ratatui renderers receive identical clipped semantic cells/styles.
- Scheduler coalesces repeated state invalidations and forces focus/cursor redraw.
- Fake terminal verifies every successful setup step rolls back in reverse order after each injected failure; repeated `stop` is harmless.
- Runner-owned state preserves editor text, viewport/follow state, component IDs, overlays, and focus across renderer suspend/resume without emitting session abort/close; renderer-private state is optional and family-specific.

```bash
cargo fmt --all -- --check
cargo test -p e-agent-tui
cargo check --workspace
```

### Review gate and rollback

- Only `ratatui/**` imports Ratatui/Crossterm.
- Fake renderer/driver establish real test uses for `Renderer` and `TerminalDriver`; built-in and test components establish the `Component` use.
- Current alternate-screen facade remains the default and all Stage 0 behavior gates pass.
- **Rollback:** keep the new neutral data but have `run` construct the Ratatui implementation unconditionally; do not expose mode switching yet.

## 5. Stage 3 — Add the versioned UI protocol, capability model, and broker

**Depends on:** Stage 2.

### Changes

- Add protocol version, immutable capability snapshots, per-operation `Native`/`Adapted`/`Degraded`/`Unsupported` support, requests/replies, extension identity, request and generation IDs, overlay/component handles, contributions, diagnostics, and cancellation data.
- Add bounded `UiBrokerClient`/`UiBrokerServer` channels; these are concrete handles, not traits.
- Service broker messages alongside session events, terminal input, timers, render wakeups, and shutdown in `runner.rs`.
- Serialize modal focus per extension, retain send order for non-coalescible operations, latest-value coalesce keyed status/widget/title/working/frame updates, bound notification FIFO, and disable slow raw-input subscribers with diagnostics.
- Tie cancellation to explicit cancel, deadline, extension unload, frontend shutdown, and handle/component disposal. Reject stale generation handles.
- Start with every optional operation explicitly unsupported. This stage must not change native TUI presentation.

### Likely files

- `e-agent-tui/src/ui_protocol.rs`
- `e-agent-tui/src/broker.rs`
- `e-agent-tui/src/state.rs`
- `e-agent-tui/src/reducer.rs`
- `e-agent-tui/src/runner.rs`
- `e-agent-tui/src/lib.rs`
- `e-agent-tui/Cargo.toml`

### Minimal tests

- Version-major rejection, additive minor-version defaults at construction boundaries, and typed unknown-operation `UnsupportedOperation` replies.
- Request/reply correlation, per-extension order, modal `Busy`, cancellation/timeout, late-reply discard, shutdown, and unload cleanup.
- Latest-value coalescing, notification overflow policy, bounded raw-input subscriber failure, and one diagnostic per overload incident.
- Keyed replacement and generation-safe stale handles.
- Session events continue while a modal or JS-side callback reply is pending.
- Every unavailable operation produces `Unsupported` with a reason; none hang or silently acknowledge.

```bash
cargo fmt --all -- --check
cargo test -p e-agent-tui broker
cargo test -p e-agent-tui
cargo check --workspace
```

### Review gate and rollback

- Protocol types contain no pi names, JavaScript values, ANSI strings, Ratatui cells, or Crossterm events.
- Broker state is extension-owned and frontend shutdown resolves all pending requests.
- **Rollback:** omit the broker server from `InteractiveOptions`; capabilities remain explicitly unavailable and native TUI behavior remains unchanged.

## 6. Stage 4 — Wire PiCompat to a fake/native UI broker without enabling custom rendering

**Depends on:** Stage 3.

### Changes

- Add a pi `0.84.2` adapter configuration carrying mode, broker client, and capability snapshot into `PiCompat` before extension/session construction.
- Replace hard-coded `mode="print", hasUI=false` with explicit interactive or headless configuration.
- Translate existing UI hostcalls (`select`, `confirm`, `input`, `editor`, `notify`, status/widget/title/editor text) into typed broker operations and correlated replies.
- Remove acknowledge-and-drop behavior. In headless/unsupported/shutdown paths map replies to documented pi defaults (`undefined` or `false`) without hanging.
- Keep JS callbacks on the node runtime thread; only bounded commands and serializable protocol values cross threads.
- Replace the unversioned custom-UI assertion/matrix with exact pi target `0.84.2` and operation-level support.
- CLI may construct a fake/no-op broker for tests, but production interactive wiring remains capability-disabled until the native components exist.

### Likely files

- `e-agent-pi-compat/src/lib.rs`
- `e-agent-pi-compat/src/ui.rs` (adapter translation and defaults)
- `e-agent-pi-compat/Cargo.toml` (add renderer-neutral `e-agent-tui`; no Ratatui/Crossterm)
- `e-agent-node-runtime/src/runtime.rs`
- `e-agent-node-runtime/src/lib.rs`
- `e-agent-cli/src/main.rs`
- `e-agent-cli/Cargo.toml`
- `e-agent-pi-compat/tests/pi_ui_fixtures.rs`

### Minimal tests

- Fake interactive broker receives exact extension ID/op/payload and returns dialog values.
- Timeout and AbortSignal for select/confirm/input, plus unsupported/unload/shutdown, resolve documented defaults; editor has cancel only because pi 0.84.2 exposes no options argument.
- Fire-and-forget calls preserve order and payload instead of being dropped.
- `ctx.mode`/`ctx.hasUI` are correct in TUI and headless modes.
- A pending modal does not deadlock the session/frontend loop.

```bash
cargo fmt --all -- --check
cargo test -p e-agent-node-runtime
cargo test -p e-agent-pi-compat
cargo test -p e-agent-tui
cargo check --workspace
```

### Review gate and rollback

- `e-agent-pi-compat` depends only on neutral TUI protocol/client types; `e-agent-node-runtime` remains independent of TUI crates.
- Headless behavior is explicit and all promises settle.
- **Rollback:** CLI omits the broker endpoint and PiCompat advertises print/headless support; never restore dropped-success behavior.

## 7. Stage 5 — Implement native Ratatui interaction capabilities

**Depends on:** Stages 2–4.

Enable one capability only after its tests pass. Recommended internal order is focus/input, dialogs, slots/contributions, then terminal behavior.

### Changes

1. Central focus manager and dispatch order: raw middleware (when negotiated), capturing overlay, active editor/selector, keymap command, app fallback. Esc/Ctrl-C are no longer unconditional before focused UI.
2. Replaceable multiline editor with text/history/change/submit/paste/autocomplete operations and semantic IME cursor anchor.
3. Select/confirm/input/editor dialogs; custom non-overlay UI temporarily replaces editor and restores text/focus/disposal.
4. Overlay stack with anchors, percentage sizing, margins, responsive visibility, non-capturing mode, z-order/focus, hide versus reversible hidden state, and generation-safe handles.
5. Header startup/transcript content, footer, above/below-editor widgets, keyed status, working indicator/message/visibility, hidden-thinking label, notifications, and title effect.
6. Semantic themes and generation invalidation of native Markdown/component caches; concrete keybinding map with namespaced commands and conflict reporting.
7. Explicit resize/paste/focus/key-release/mouse handling, follow/scroll behavior, baseline selection/copy/paste and OSC 8 links, with capability-backed degradation.
8. Main-screen and alternate-screen rendering over the same snapshots. Do not enable runtime switching until Stage 8 preservation tests pass.

### Likely files

- `e-agent-tui/src/{state.rs,reducer.rs,input.rs,component.rs,render.rs,runner.rs,ui_protocol.rs,broker.rs}`
- `e-agent-tui/src/ratatui/{renderer.rs,layout.rs,input.rs,terminal.rs}`
- `e-agent-tui/src/ratatui/components/{editor.rs,dialogs.rs,selector.rs,overlay.rs,slots.rs,status.rs,notification.rs,transcript.rs,tool.rs}`
- `e-agent-tui/src/theme.rs`
- `e-agent-tui/src/keybindings.rs`

### Minimal tests

- Input precedence, representative remap/conflict, selector/dialog submit/cancel, timeout, editor replacement/restore/dispose, and multiline/large paste with no literal bracket markers.
- Nested overlay focus/z-order, responsive hide/show, non-capturing behavior, explicit unfocus target, stale handle, CJK IME cursor, and pinned 0.84.2 topmost-overlay close behavior.
- Slot key replacement/removal/disposal, footer data refresh, multiple statuses, default reset, hidden/static/animated working states, notification ordering/overflow.
- Theme generation rebuilds cached Markdown and component content once.
- Buffer snapshots for every new view at narrow/normal/wide widths while all Stage 0 transcript/tool/Markdown/editor fixtures stay green.
- Resize reflow, follow break/resume, wheel/key scroll, selection/link hit maps, title support and unsupported reports.
- Main and alternate modes consume equivalent snapshots; mode-specific capabilities are accurate.

```bash
cargo fmt --all -- --check
cargo test -p e-agent-tui
cargo test -p e-agent-pi-compat
cargo check --workspace
```

### Review gate and rollback

- Turn on each operation in `UiCapabilities` only with its deterministic acceptance test.
- Renderer/component failures remain presentation failures and do not manufacture core events.
- **Rollback:** mark only the regressed operation degraded/unsupported and use its documented default/native fallback; keep the rest enabled. Keep alternate mode as current default until mode switching is proven.

## 8. Stage 6 — Complete the isolated pi 0.84.2 compatibility adapter

**Depends on:** Stage 5 native capabilities and Stage 4 broker wiring.

### Changes

- Extend node-runtime commands/accessors only as needed to retain and invoke registered shortcuts, autocomplete/editor factories, terminal-input handlers, custom tool call/result renderers, message/entry renderers, Markdown transformers, header/footer/widget/custom component factories, and disposal callbacks.
- Split pi components across the thread boundary: a node-runtime `PiComponentProducer` owns/invokes JS callbacks and returns owned ANSI lines; the pi adapter parses them into immutable semantic frames, and a runner-owned `PiCachedComponent` renders only the latest frame and sends bounded input/invalidation/render requests. No renderer call synchronously invokes JS.
- Encode normalized input back to pinned pi raw/Kitty strings only for pi-focused components; negotiate release support. Apply raw listeners in registration order with observe/consume/transform/unsubscribe semantics and bounded re-decoding.
- Implement stable pi TUI proxy IDs/handles, custom non-overlay and overlay lifecycle, async factory/done/dispose behavior, replaceable editor composition, theme/keybinding objects, widgets/header/footer, status/working APIs, editor text/paste/autocomplete, tool expansion, and title.
- Preserve tool renderer `toolCallId`, partial/expanded/error flags, `state`, `lastComponent`, and shell policy. Catch per-row failures and use native fallback.
- Run message/entry renderer and Markdown transformer chains with per-item failure isolation; a failing transformer preserves output so far and continues.
- Add callback deadlines. A throw or late callback removes only the owning contribution, restores focus/editor, settles its request, and emits a diagnostic. A deadline cannot preempt a synchronous infinite loop on the single JS runtime thread; test frontend responsiveness separately and isolate any true infinite-loop test in a child process.
- Report terminal images and undocumented internals as unsupported; do not invent general clipboard APIs absent from public pi `0.84.2`.

### Likely files

- `e-agent-node-runtime/src/runtime.rs`
- `e-agent-node-runtime/src/lib.rs`
- `e-agent-pi-compat/src/lib.rs`
- `e-agent-pi-compat/src/ui.rs`
- `e-agent-pi-compat/src/ansi.rs`
- `e-agent-pi-compat/src/input.rs`
- `e-agent-pi-compat/src/renderers.rs`
- `e-agent-pi-compat/tests/pi_ui_fixtures.rs`
- `e-agent-pi-compat/tests/fixtures/pi-0.84.2/**`
- `e-agent-tui/src/ui_protocol.rs` only for previously reviewed generic protocol fields, never pi-specific types

### Minimal tests

- ANSI/CJK width, clipping, style/hyperlink reset, cursor marker, unsafe control sequence sanitization.
- Raw input observe/consume/transform/unsubscribe/rebind and release downgrade.
- Component factory rejection, idempotent `done`, disposal error, stale handle, callback throw/late reply, and unload during modal; a child-process test may demonstrate the documented non-preemptible infinite-loop limit without hanging the suite.
- Tool partial/expanded/state/`lastComponent`/self-shell/throw fallback.
- Message/entry persistence/restore and renderer throw fallback.
- Markdown chain, failure continuation, streaming, resize, and incomplete source.
- Theme switch rebuilds native and ANSI caches.

```bash
cargo fmt --all -- --check
cargo test -p e-agent-node-runtime
cargo test -p e-agent-pi-compat
cargo test -p e-agent-tui
cargo check --workspace
```

### Review gate and rollback

- Pi, ANSI, raw terminal strings, AbortSignal, and JS closure types occur only in `e-agent-pi-compat`/`e-agent-node-runtime` or edge tests.
- Every capability-matrix row has an exact support level and fallback.
- **Rollback:** downgrade the individual adapter operation while retaining native components and explicit defaults; the stable session runner remains unchanged.

## 9. Stage 7 — Split the concrete Ratatui crate and expose CLI compile-time composition

**Depends on:** Stages 1–6 are green inside the temporary single-crate layout.

### Changes

- Create `e-agent-tui-api` and move the renderer-neutral runner, state, semantic frame/component contracts, broker, protocol, `InteractiveFrontend`, and `InteractiveOptions` into it.
- Create `e-agent-tui-ratatui` and move terminal, Crossterm input decoding, Ratatui rendering/layout/Markdown, and built-in concrete components into it; it depends on `e-agent-tui-api`.
- Convert the existing `e-agent-tui` package into the thin compatibility facade that depends on both crates and implements `e_agent_tui::run(SessionAttachment)` by constructing the reference frontend. This is the required permanent packaging shape; no temporary re-export can solve the Cargo cycle.
- Remove Ratatui, Crossterm, and `pulldown-cmark` from `e-agent-tui-api`; the facade may transitively link them by design.
- Update workspace members and CLI dependencies. CLI creates broker/client before `PiCompat` and session construction, selects a linked frontend factory, and runs it inside the existing `LocalSet`.
- Add a minimal second/fake frontend in tests only; no dynamic library, external process, or runtime plugin system.

### Likely files

- `Cargo.toml`
- `e-agent-tui/Cargo.toml`
- `e-agent-tui/src/lib.rs`
- `e-agent-tui-ratatui/Cargo.toml`
- `e-agent-tui-ratatui/src/{lib.rs,terminal.rs,input.rs,renderer.rs,layout.rs,markdown.rs}`
- `e-agent-tui-ratatui/src/components/**`
- `e-agent-tui-ratatui/tests/render_contract.rs`
- `e-agent-cli/Cargo.toml`
- `e-agent-cli/src/main.rs`
- `e-agent-cli/tests/frontend_selection.rs` or an equivalent unit test module

### Minimal tests

- Dependency checks prove `e-agent-tui-api` has no Ratatui/Crossterm/pi/ANSI/JS dependency and core remains unchanged; `e-agent-tui` is expected to reach Ratatui through its legacy facade dependency.
- Ratatui and fake/second frontends receive the same initial snapshot and event/effect sequence.
- Compile-time selection changes only CLI construction; neither core nor orchestration is duplicated.
- Exactly one attachment is consumed and frontend replacement/switch does not abort or close it.
- Legacy `e_agent_tui::run(SessionAttachment)` still compiles and follows the reference frontend.

```bash
cargo fmt --all -- --check
cargo test -p e-agent-tui-api
cargo test -p e-agent-tui
cargo test -p e-agent-tui-ratatui
cargo test -p e-agent --tests
cargo check --workspace
cargo tree -p e-agent-tui-api
```

### Review gate and rollback

- `cargo tree -p e-agent-tui-api` contains no Ratatui/Crossterm; `cargo tree -p e-agent-core` contains no TUI/terminal/pi dependency.
- The CLI defaults to the Ratatui frontend.
- **Rollback:** select Ratatui unconditionally at the CLI composition root. Do not revert neutral contracts, duplicate the runner, or change core.

## 10. Stage 8 — Enable renderer/mode replacement and harden lifecycle

**Depends on:** Stage 7 crate split and replacement contract tests.

### Changes

- Implement preflight/suspend/stop-old/start-new/resume/invalidate/publish-capabilities/rebind-input. Because terminal ownership cannot be atomic, restart the old renderer best-effort on failure; if rollback fails, restore the terminal, settle pending requests, and exit cleanly.
- Preserve component IDs, editor text, overlays/handles, focus, viewport/follow state, persistent contributions, and stable pi TUI proxy identity.
- Reject switching during an incompatible modal transition with a typed reason; never partially switch.
- Distinguish renderer failure from session fatal state; recover through configured reference fallback where safe.
- Complete signal and panic/unwind coordination: extension/runtime cleanup ordering, bounded input drain, idempotent terminal restoration, dead-output behavior, and previous panic-hook delegation.
- Keep mode/concrete-renderer replacement separate from application exit so no switch emits session abort/close.

### Likely files

- `e-agent-tui/src/{runner.rs,render.rs,state.rs,broker.rs,ui_protocol.rs}`
- `e-agent-tui-ratatui/src/{lib.rs,renderer.rs,terminal.rs}`
- `e-agent-cli/src/main.rs`
- `e-agent-tui/tests/replacement_contract.rs`
- `e-agent-tui-ratatui/tests/terminal_lifecycle.rs`

### Minimal tests

- Fake-to-fake and main-to-alternate switch preserve all renderer-neutral state and perform one full redraw.
- Capability changes publish once; raw listeners rebind without duplicate input.
- Preflight failure leaves the old renderer untouched; post-stop construction/resume failure either restarts it from runner-owned state or exits after restoration; pending requests settle.
- Normal exit, session abort/fatal, renderer error, signal, and panic/unwind all call idempotent restoration; replacement calls neither session abort nor close.

```bash
cargo fmt --all -- --check
cargo test -p e-agent-tui replacement
cargo test -p e-agent-tui-ratatui terminal_lifecycle
cargo test --workspace
```

### Review gate and rollback

- Replacement contract passes with two implementations and both screen modes.
- **Rollback:** disable switching and select the main/reference or existing alternate/reference renderer at CLI construction; keep the stable runner and broker alive.

## 11. Stage 9 — Final unchanged TypeScript compatibility and real-terminal acceptance

**Depends on:** all previous stages.

### Unchanged fixture acceptance

Run the Stage 0 copies byte-for-byte unchanged through the real node runtime, Pi adapter, broker, stable runner, and deterministic Ratatui backend. The harness may supply deterministic key/input/resize/theme/capability scripts but may not rewrite fixture source.

The suite must cover:

- select/confirm/input/editor submit and cancel; timeout and AbortSignal defaults for select/confirm/input only;
- overlay QA anchors, stacking, responsive visibility, hide/show, focus/unfocus, and non-capturing panels;
- modal editor wrapping/restoration, autocomplete delegation, editor text and large multiline paste;
- custom header/footer/widgets and disposal/data refresh;
- status and working indicators, notifications, title, themes, keybindings, and terminal input consume/transform/unsubscribe;
- custom tool call/result partial/expanded/state/`lastComponent`/fallback;
- custom message and persisted entry rendering;
- Markdown transformers during restored, streaming, finalized, and resized rendering;
- renderer/mode replacement with stable proxy/handles and no session reattachment;
- unsupported images/advanced platform edges returning versioned capability results.

Use Rust mocks only for fault injection; they do not replace these fixtures.

### Minimal real-terminal smoke tests

Add a PTY-backed smoke harness where the platform supports it. Verify:

- transactional failure rollback during setup;
- normal, abort/fatal, signal, and panic/unwind restoration;
- main/alternate enter/leave and cursor visibility/IME placement;
- bracketed paste, resize, mouse wheel/selection, OSC 8 links, text copy fallback, and slow-input drain;
- terminal-unavailable features appear degraded/unsupported in capabilities rather than silently skipping assertions.

On Windows or another platform without a suitable PTY feature, run the fake-driver lifecycle matrix and record the real-terminal smoke as an explicit capability/platform limitation, not an unreported skip.

### Likely files

- `e-agent-pi-compat/tests/pi_ui_compat.rs`
- `e-agent-pi-compat/tests/fixtures/pi-0.84.2/**`
- `e-agent-tui/tests/replacement_contract.rs`
- `e-agent-tui-ratatui/tests/render_contract.rs`
- `e-agent-tui-ratatui/tests/terminal_smoke.rs`
- `e-agent-tui-ratatui/Cargo.toml` dev-dependencies/features only if the PTY test cannot use existing facilities
- CI configuration only if this repository already has an established CI location; do not add a new CI framework solely for this task

### Final validation commands

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo test -p e-agent-pi-compat --test pi_ui_compat
cargo test -p e-agent-tui --test replacement_contract
cargo test -p e-agent-tui-ratatui --test render_contract
cargo test -p e-agent-tui-ratatui --test terminal_smoke -- --nocapture
cargo tree -p e-agent-core
cargo tree -p e-agent-tui-api
```

If Clippy is available in the project toolchain, also run:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

### Final review gate

- Every M1 row in `design.md` has a deterministic native test and, where extension-facing, an unchanged pi TypeScript fixture.
- Transcript, streaming assistant/thinking/tool, Markdown, multiline editor, scrolling/follow mode, abort/fatal, CJK, and terminal cleanup behavior remain preserved.
- A second frontend is selected by CLI composition without modifying `e-agent-core` or duplicating session orchestration.
- Core and `e-agent-tui-api` dependency trees satisfy the design; the legacy `e-agent-tui` facade intentionally links the reference renderer.
- All setup/teardown paths restore terminal state where the platform supports them; unavailable paths are explicit capabilities.
- Fixture hashes still match Stage 0.
- The `e-agent-tool-ptc/src/ptc/mod.rs` patch fingerprint still matches the captured baseline and the path is absent from this task's staged/committed files.

**Final rollback:** retain the stable runner/protocol but select the Ratatui reference frontend and known-good screen mode at the CLI; downgrade only failing pi operations with explicit defaults. Rollback must never require a core edit, a second attachment, or restoration of the unrelated PTC file.

## 12. Completion checklist

- [ ] Stages 0–9 passed in order, with each stage buildable before continuing.
- [ ] `e_agent_tui::run(SessionAttachment)` migration facade remains available.
- [ ] Only reviewed replacement/test interfaces were added.
- [ ] Reducer, broker, renderer, component/focus, terminal lifecycle, and replacement contracts have deterministic tests.
- [ ] Minimal PTY smoke coverage exists where supported, with explicit capability results elsewhere.
- [ ] Pi `0.84.2` fixtures remain unchanged and cover the documented public compatibility surface.
- [ ] Images, undocumented internals, and unneeded advanced protocols remain rejected/deferred and explicitly reported.
- [ ] Workspace formatting, checks, tests, and dependency audits pass.
- [ ] Unrelated `e-agent-tool-ptc/src/ptc/mod.rs` work is untouched.
- [ ] User reviews and approves `prd.md`, `design.md`, and this plan before `task.py start` or production implementation begins.
