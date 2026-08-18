# Pi 0.84.2 TUI and Extension UI Contract Research

## Scope and version

Installed package inspected: `@earendil-works/pi-coding-agent` **0.84.2**. This report distinguishes:

1. the documented extension compatibility surface that existing TypeScript extensions can depend on;
2. pi-tui's public component/terminal contracts;
3. pi's internal composition and lifecycle behavior where it materially constrains compatibility; and
4. renderer-neutral contracts appropriate for e-agent versus behavior that belongs only in a pi/ANSI compatibility adapter.

No production code was changed.

## Primary evidence (exact local paths)

Documentation read completely:

- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\docs\extensions.md`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\docs\tui.md`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\docs\themes.md`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\docs\keybindings.md`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\docs\terminal-setup.md`

Authoritative declarations and implementation inspected:

- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\dist\core\extensions\types.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\dist\core\keybindings.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\dist\core\footer-data-provider.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\dist\modes\interactive\interactive-mode.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\dist\modes\interactive\interactive-mode.js`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\dist\modes\interactive\components\custom-editor.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\dist\modes\interactive\theme\theme.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\node_modules\@earendil-works\pi-tui\dist\tui.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\node_modules\@earendil-works\pi-tui\dist\terminal.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\node_modules\@earendil-works\pi-tui\dist\tui-main-screen.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\node_modules\@earendil-works\pi-tui\dist\tui-alt-screen.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\node_modules\@earendil-works\pi-tui\dist\editor-component.d.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\node_modules\@earendil-works\pi-tui\dist\keybindings.d.ts`

Representative compatibility fixtures inspected:

- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\examples\extensions\overlay-qa-tests.ts`
- `C:\Users\firespoon\.nvm\versions\node\v25.6.0\bin\node_modules\@earendil-works\pi-coding-agent\examples\extensions\custom-header.ts`
- Documentation also identifies `custom-footer.ts`, `modal-editor.ts`, `widget-placement.ts`, `working-indicator.ts`, `github-issue-autocomplete.ts`, `message-renderer.ts`, `entry-renderer.ts`, `todo.ts`, and `mac-system-theme.ts` as canonical examples for their respective APIs.

## 1. Complete documented `ctx.ui` surface and signatures

The declaration in `dist/core/extensions/types.d.ts` is authoritative. `ctx.mode` is `"tui" | "rpc" | "json" | "print"`; `ctx.hasUI` is true for TUI and RPC. TUI-only APIs must be guarded with `ctx.mode === "tui"`. In RPC, dialogs/notifications are protocol-backed but `custom()` returns `undefined`; JSON and print UI methods are no-ops/defaults.

### Dialogs and notifications

```ts
interface ExtensionUIDialogOptions {
  signal?: AbortSignal;
  timeout?: number; // milliseconds; live countdown
}

select(title: string, options: string[], opts?: ExtensionUIDialogOptions): Promise<string | undefined>;
confirm(title: string, message: string, opts?: ExtensionUIDialogOptions): Promise<boolean>;
input(title: string, placeholder?: string, opts?: ExtensionUIDialogOptions): Promise<string | undefined>;
editor(title: string, prefill?: string): Promise<string | undefined>;
notify(message: string, type?: "info" | "warning" | "error"): void;
```

Timeout/abort/cancel results are `undefined` for select/input/editor and `false` for confirm. The documented `editor()` has no dialog options in 0.84.2. Notifications are non-blocking and become transcript status/error/warning content in interactive mode, not OS desktop notifications.

### Raw terminal input

```ts
type TerminalInputHandler = (data: string) =>
  | { consume?: boolean; data?: string }
  | undefined;

onTerminalInput(handler: TerminalInputHandler): () => void;
```

This is interactive-only. The unsubscribe function is part of the contract. A listener can observe, consume, or transform raw terminal data before focused-component dispatch. This is explicitly raw/ANSI-oriented and should not contaminate native Rust input contracts.

### Status and streaming indicators

```ts
setStatus(key: string, text: string | undefined): void;
setWorkingMessage(message?: string): void;
setWorkingVisible(visible: boolean): void;
setWorkingIndicator(options?: {
  frames?: string[];
  intervalMs?: number;
}): void;
setHiddenThinkingLabel(label?: string): void;
```

- Status is keyed, persistent, and cleared with `undefined`.
- Omitting working message/indicator restores pi defaults.
- `frames: []` hides the indicator; one frame is static; frames are rendered verbatim and must contain their own ANSI theme styling.
- `setWorkingVisible(false)` hides the whole normal streaming loader row; compaction/retry loaders retain their built-in presentation.

### Widgets, header, footer, title

```ts
type WidgetPlacement = "aboveEditor" | "belowEditor";
interface ExtensionWidgetOptions { placement?: WidgetPlacement }

setWidget(key: string, content: string[] | undefined,
          options?: ExtensionWidgetOptions): void;
setWidget(key: string,
          content: ((tui: TUI, theme: Theme) => Component & { dispose?(): void }) | undefined,
          options?: ExtensionWidgetOptions): void;

setFooter(factory: ((tui: TUI, theme: Theme,
  footerData: ReadonlyFooterDataProvider) => Component & { dispose?(): void }) | undefined): void;

setHeader(factory: ((tui: TUI, theme: Theme) =>
  Component & { dispose?(): void }) | undefined): void;

setTitle(title: string): void;
```

Widgets default above the editor and are keyed for replacement/removal. Footer replacement is total. `ReadonlyFooterDataProvider` exposes:

```ts
getGitBranch(): string | null;
getExtensionStatuses(): ReadonlyMap<string, string>;
getAvailableProviderCount(): number;
onBranchChange(callback: () => void): () => void;
```

The header is startup/transcript content above chat, not a fixed viewport chrome requirement. Component factories may return `dispose()`; replacement/reset must invoke cleanup. Passing `undefined` restores the built-in header/footer or removes the widget.

### Custom component / overlay

```ts
custom<T>(
  factory: (
    tui: TUI,
    theme: Theme,
    keybindings: KeybindingsManager,
    done: (result: T) => void
  ) => (Component & { dispose?(): void }) |
       Promise<Component & { dispose?(): void }>,
  options?: {
    overlay?: boolean;
    overlayOptions?: OverlayOptions | (() => OverlayOptions);
    onHandle?: (handle: OverlayHandle) => void;
  }
): Promise<T>;
```

Without overlay, custom UI temporarily replaces the editor, captures focus, preserves editor text, and restores editor/focus after `done`. With overlay it composites over existing content. `done` is idempotently closed by implementation. `dispose()` is called after successful close and errors from disposal are ignored. Factory rejection restores the editor for non-overlay mode and rejects the promise. A component is stale after close and must not be reused.

A material 0.84.2 implementation edge: `showExtensionCustom()` closes an overlay via `ui.hideOverlay()`, which hides the topmost overlay rather than retaining the returned handle in the closure. Representative stacking fixtures should verify close ordering before claiming exact compatibility.

### Editor text, paste, autocomplete, editor replacement

```ts
pasteToEditor(text: string): void;
setEditorText(text: string): void;
getEditorText(): string;
addAutocompleteProvider(factory: (current: AutocompleteProvider) => AutocompleteProvider): void;

type EditorFactory =
  (tui: TUI, theme: EditorTheme, keybindings: KeybindingsManager) => EditorComponent;
setEditorComponent(factory: EditorFactory | undefined): void;
getEditorComponent(): EditorFactory | undefined;
```

`pasteToEditor` injects bracketed-paste semantics (`ESC[200~...ESC[201~`) so large-paste collapsing and editor paste handling run. `setEditorText` directly replaces core editor content. `getEditorText` returns expanded text when supported. There is no general documented `ctx.ui.readClipboard()`/`writeClipboard()` API; compatibility for “text clipboard operations” means paste-to-editor plus application copy/paste behavior, not an invented extension clipboard service.

Autocomplete providers wrap the current provider and should delegate when their trigger does not match. Editor factories can wrap the prior factory obtained through `getEditorComponent()`.

`EditorComponent` requires:

```ts
interface EditorComponent extends Component {
  getText(): string;
  setText(text: string): void;
  handleInput(data: string): void;
  onSubmit?: (text: string) => void;
  onChange?: (text: string) => void;
  addToHistory?(text: string): void;
  insertTextAtCursor?(text: string): void;
  getExpandedText?(): string;
  setAutocompleteProvider?(provider: AutocompleteProvider): void;
  borderColor?: (str: string) => string;
  setPaddingX?(padding: number): void;
  setAutocompleteMaxVisible?(maxVisible: number): void;
}
```

Pi instructs extension editors to derive from coding-agent `CustomEditor`, not bare pi-tui `Editor`, and pass unknown keys to `super.handleInput()` to preserve app interrupt, exit, model, external editor, image paste, and extension shortcuts. `CustomEditor` adds action handlers, `onEscape`, `onCtrlD`, `onPasteImage`, and `onExtensionShortcut`.

### Theme and tool expansion state

```ts
readonly theme: Theme;
getAllThemes(): { name: string; path: string | undefined }[];
getTheme(name: string): Theme | undefined;
setTheme(theme: string | Theme): { success: boolean; error?: string };
getToolsExpanded(): boolean;
setToolsExpanded(expanded: boolean): void;
```

`Theme` provides `fg`, `bg`, `bold`, `italic`, `underline`, `inverse`, `strikethrough`, ANSI getters, color-mode inspection, and thinking/bash border functions. Theme changes invalidate all components. Components that cache pre-styled strings must rebuild them in `invalidate()`, not merely clear line caches.

## 2. Component, Focusable, TUI, and Terminal contracts

### Component

```ts
interface Component {
  render(width: number): string[];
  handleInput?(data: string): void;
  wantsKeyRelease?: boolean;
  invalidate(): void;
}
```

Every rendered line must be at most `width` display cells, measured ignoring ANSI. Pi appends full SGR and OSC 8 resets to each line, so styles/hyperlinks do not carry across lines. `wantsKeyRelease` opts into Kitty key-release events; release events are otherwise filtered. `invalidate` is both cache invalidation and theme-change notification.

Utilities extensions rely on include `visibleWidth`, `truncateToWidth`, and `wrapTextWithAnsi`. The built-in component vocabulary includes `Text`, `Box`, `Container`, `Spacer`, `Markdown`, `Image`, `SelectList`, `SettingsList`, `Input`, and `Editor`.

### Focusable and IME

```ts
interface Focusable { focused: boolean }
const CURSOR_MARKER = "\x1b_pi:c\x07";
```

When focus changes, TUI sets `focused`. A focused input emits the zero-width marker immediately before its fake cursor. The renderer strips it, computes cell coordinates, and positions the hardware terminal cursor. Hardware cursor visibility is separately controlled and defaults hidden; `PI_HARDWARE_CURSOR=1` or `setShowHardwareCursor(true)` supports terminals that require visibility for IME candidates. Containers embedding an input must implement `Focusable` and propagate `focused` to the child. This is a semantic cursor-anchor contract; the APC byte marker itself belongs only in the compatibility adapter.

### TUI

Public `TUI` extends `Component` and exposes:

```ts
readonly mode: "regular" | "fullscreen";
children: Component[];
terminal: Terminal;
onDebug?: () => void;
readonly fullRedraws: number;
addChild(component): void; removeChild(component): void; clear(): void;
getShowHardwareCursor(): boolean; setShowHardwareCursor(enabled): void;
getClearOnShrink(): boolean; setClearOnShrink(enabled): void;
setFocus(component: Component | null): void;
showOverlay(component, options?): OverlayHandle;
hideOverlay(): void; hasOverlay(): boolean;
start(): void; stop({ preserveScreen? }?): void;
renderNow(force?: boolean): void; requestRender(force?: boolean): void;
addInputListener(listener): () => void; removeInputListener(listener): void;
onTerminalColorSchemeChange(listener): () => void;
setTerminalColorSchemeNotifications(enabled: boolean): void;
queryTerminalBackgroundColor({ timeoutMs }): Promise<RgbColor | undefined>;
queryTerminalColorScheme({ timeoutMs }): Promise<"dark" | "light" | undefined>;
```

Rendering is scheduled/coalesced; components call `requestRender()` after state change. `renderNow()` is the synchronous/forced path. Theme invalidation and render scheduling are distinct operations.

### Terminal

```ts
interface Terminal {
  start(onInput: (data: string) => void, onResize: () => void): void;
  stop(): void;
  drainInput(maxMs?: number, idleMs?: number): Promise<void>;
  write(data: string): void;
  readonly columns: number;
  readonly rows: number;
  readonly kittyProtocolActive: boolean;
  moveBy(lines: number): void;
  hideCursor(): void; showCursor(): void;
  clearLine(): void; clearFromCursor(): void; clearScreen(): void;
  setTitle(title: string): void;
  setProgress(active: boolean): void;
}
```

`ProcessTerminal` owns raw mode, stdin sequence buffering, resize callbacks, cursor/screen operations, Kitty progressive keyboard negotiation, modifyOtherKeys fallback, Windows VT input, bracketed paste, and teardown. `drainInput` prevents delayed Kitty release sequences leaking into the parent shell over slow SSH.

Terminal limitations documented by pi must become negotiated capabilities, not silent promises: Kitty event types/super modifiers, modified Enter, mouse capture, truecolor, hardware-cursor IME, clipboard transport, and platform job control vary by terminal/platform.

## 3. Overlay contract, focus, and lifecycle

### Options

```ts
type OverlayAnchor =
  | "center" | "top-left" | "top-right" | "bottom-left" | "bottom-right"
  | "top-center" | "bottom-center" | "left-center" | "right-center";
type SizeValue = number | `${number}%`;
interface OverlayOptions {
  width?: SizeValue;
  minWidth?: number;
  maxHeight?: SizeValue;
  anchor?: OverlayAnchor;
  offsetX?: number;
  offsetY?: number;
  row?: SizeValue;
  col?: SizeValue;
  margin?: number | { top?: number; right?: number; bottom?: number; left?: number };
  visible?: (termWidth: number, termHeight: number) => boolean;
  nonCapturing?: boolean;
}
```

Default anchor is center. Explicit row/column and percentage dimensions are supported. `visible` is evaluated per render. `nonCapturing` permits passive panels.

### Handle

```ts
interface OverlayHandle {
  hide(): void;                  // permanent removal
  setHidden(hidden: boolean): void;
  isHidden(): boolean;
  focus(): void;                 // input owner + visual front
  unfocus(options?: { target: Component | null }): void;
  isFocused(): boolean;
}
```

Focus order is also z-order: focusing brings an overlay to the visual front. A focused visible overlay keeps ownership across temporary non-overlay custom UI and can reclaim focus when that UI closes. `unfocus()` falls back to the next visible capturing overlay or previous target; explicit target selects another component, and `target: null` deliberately leaves no focus. Hidden/non-visible/dismissed overlays must not retain input. `hide()` is permanent. `setHidden` is reversible.

Overlay close disposes the component. References are stale afterward. Nested/stacked overlays, responsive hiding, non-capturing panels, per-panel dismissal, and focus cycling are exercised in `examples/extensions/overlay-qa-tests.ts` and should be imported as compatibility fixtures rather than represented only by Rust mocks.

## 4. Custom rendering and extension-authored UI

### Tool renderers

```ts
renderShell?: "default" | "self";
renderCall?: (args, theme, context) => Component;
renderResult?: (result, { expanded, isPartial }, theme, context) => Component;
```

`ToolRenderContext<TState,TArgs>` contains `args`, stable `toolCallId`, row-local `invalidate()`, per-slot `lastComponent`, shared `state`, `cwd`, `executionStarted`, `argsComplete`, `isPartial`, `expanded`, `showImages`, and `isError`.

Default shell is a state-colored box. `renderShell: "self"` transfers framing/padding/background responsibility to the extension. Call and result slots inherit/fallback independently for built-in overrides. Missing or throwing call renderer falls back to tool name; missing or throwing result renderer falls back to raw text content. Renderers should reuse `lastComponent`, handle partial updates, and support expanded/collapsed state. Renderer failure must be isolated to the row and must not break the session or renderer.

### Message, entry, and Markdown contribution points

Documented extension registration also includes:

- `pi.registerMessageRenderer(customType, renderer)` for custom messages that participate in model context;
- `pi.registerEntryRenderer(customType, renderer)` for persisted TUI-only custom entries;
- `pi.registerMarkdownTransformer(transformer)` receiving `{ messageType, isStreaming, availableWidth }` and returning Markdown.

Markdown transformer failures retain output produced so far and continue the chain. They run for restored, streaming, finalized, and resized rendering, so must be synchronous and cheap. These are part of “extension-authored custom UI,” even though they are registered on `pi`, not `ctx.ui`.

## 5. Themes, keybindings, input, mouse, and clipboard

### Themes

Themes are discovered from built-ins, global/project/package/settings/CLI resources. Project themes require trust. Active custom theme files hot reload. Theme names are unique and cannot contain `/`. Values support RGB hex, xterm-256 index, variables, and terminal default. Pi emits truecolor where available and approximates for 256-color terminals.

The public Theme token set covers core text/borders/status, message/tool backgrounds, Markdown, diffs, syntax, thinking levels, and bash mode. Optional fallback tokens are `thinkingMax`, `scrollbarThumb`, `searchMatchBg`, and `searchMatchText`. The extension-facing requirement is semantic tokens and invalidation, not pi's JSON schema or ANSI encoding in every native component.

### Keybindings

`KeybindingsManager` resolves default plus user bindings and provides:

```ts
matches(data: string, keybinding: Keybinding): boolean;
getKeys(keybinding: Keybinding): KeyId[];
getDefinition(keybinding): KeybindingDefinition;
getConflicts(): KeybindingConflict[];
setUserBindings(config): void;
getUserBindings(): KeybindingsConfig;
getResolvedBindings(): KeybindingsConfig;
```

Bindings use namespaced IDs (`tui.*`, `app.*`), one key or key array; user values replace defaults. The documented action families are editor movement/deletion/history/kill ring/undo, input submit/newline/tab/copy, selection, fullscreen transcript viewport/search, app interrupt/clear/exit/suspend/external editor/clipboard paste, session/model/thinking/tool/message queue actions, tree navigation, and scoped-model controls. Full exhaustive defaults are in `docs/keybindings.md`; compatibility fixtures should load representative remaps rather than freeze defaults into reducer logic.

Input arrives as raw terminal sequences to the pi adapter. `matchesKey`/`Key` and Kitty event decoding belong in that adapter. Native Rust input should use normalized key, text, paste, mouse, resize, focus, and capability events. `wantsKeyRelease` is negotiated per component.

### Mouse, scrolling, and clipboard

Regular/main-screen mode relies on terminal scrollback. Fullscreen/alternate-screen owns a viewport and supports wheel/trackpad scrolling, prompt jumps, transcript search, scrollbar behavior, OSC 8 link clicks, primary-button drag selection with edge auto-scroll, and optional right-click paste on Windows. Selection copy uses a supplied platform clipboard callback and falls back to OSC 52 when absent.

Application clipboard behavior also includes copy selection/editor/message and `app.clipboard.pasteImage`; `pasteToEditor(text)` is the extension text-paste primitive. Image protocols are documented in pi-tui but explicitly outside this task's compatibility target.

## 6. Main-screen versus alternate-screen composition

`createInteractiveTui(options)` is the composition root:

- `tuiMode === "fullscreen"` creates `TuiAltScreen` with search styling, URL opener, right-click paste callback, and clipboard copy callback;
- otherwise it creates `TuiMainScreen` over the same `Terminal` abstraction.

`TuiMainScreen` renders into the main screen and native scrollback with differential state that can be captured/restored. `TuiAltScreen` owns a fixed viewport/layout root, scrolling/follow state, search, mouse selection, flashes, links, scrollbar, and alternate-screen enter/exit.

InteractiveMode keeps a stable `TUI` proxy from `createInteractiveTuiReference(() => renderer)`. Every property/method dynamically delegates to the current renderer. On mode switch it:

1. rejects switching while overlays exist;
2. captures children, focus, terminal, cursor/clear settings, debug callback, and main-screen render state;
3. stops the old renderer with `preserveScreen`;
4. creates the new renderer at the composition root;
5. remounts the same component objects and fullscreen layout root;
6. invalidates and restores focus;
7. starts it and rebinds theme synchronization and extension terminal-input listeners.

This is strong evidence for e-agent's replacement seam: stable UI/controller ownership plus renderer selection at the CLI composition root. The renderer implementation can change without changing session orchestration or extension/component references.

## 7. Teardown and failure behavior

- Normal interactive quit disables theme auto-sync, drains terminal input for up to 1s, stops/restores TUI, then disposes runtime/extensions, prints resume help, and exits.
- Signal quit (`SIGTERM`, plus `SIGHUP` off Windows) disposes runtime/extensions first so `session_shutdown` is not skipped if terminal restoration hits EIO; then disables theme sync, drains, stops, and exits.
- Dead terminal stdout/stderr errors use emergency exit: unregister handlers, kill tracked detached children, avoid further TTY writes, exit 129.
- Uncaught exception unregisters handlers, kills tracked children, best-effort `ui.stop()` to restore cooked mode/cursor/keyboard modes, logs, exits 1.
- `stop()` clears status, theme sync, raw-input subscriptions, footer/watch resources, agent subscription, renderer, and signal handlers.
- `ProcessTerminal.stop()` is responsible for cursor/raw mode/bracketed paste/Kitty/modifyOtherKeys cleanup.
- Extension component `dispose()` failures are swallowed at custom UI close. Extension errors generally are logged and isolated; custom tool renderer errors use fallback rendering.

Rust should use an RAII terminal guard plus a panic hook that performs idempotent best-effort restoration. Signal handling and normal shutdown need distinct ordering only where extension/session cleanup versus a possibly dead terminal makes it necessary. Cleanup must be idempotent and tested for normal exit, abort/fatal state, signal, and unwind.

## 8. Compatibility matrix: pi adapter versus renderer-neutral Rust

| Pi 0.84.2 capability | Pi/ANSI compatibility adapter responsibility | Renderer-neutral Rust abstraction | Required acceptance evidence |
|---|---|---|---|
| `select/confirm/input/editor` | Translate TypeScript call/promise, timeout and AbortSignal semantics | Typed dialog request/result + cancellation | Representative extension fixture; cancel/timeout/default results |
| `notify` | Preserve pi severity and non-blocking call | Notification event/model | Info/warn/error render and ordering |
| `custom<T>` | Host TS component factory; bridge `done`, async factory, dispose, ANSI lines | Custom surface lifecycle and result channel | Replace-editor, async rejection, idempotent close, disposal |
| Component `render(width): string[]` | Interpret ANSI/OSC, enforce display width, reset each line | Native `View::render(area, buffer)` or equivalent | Narrow/CJK/ANSI/hyperlink fixture; overflow isolation |
| `handleInput(data)` / `wantsKeyRelease` | Encode normalized events back to pi raw sequences; Kitty release opt-in | Typed input event and propagation result | Press/repeat/release plus unsupported terminal downgrade |
| Focusable `CURSOR_MARKER` | Parse/remove APC marker and map ANSI cell location | Semantic focus + cursor anchor | Nested input propagation and CJK IME cursor tests |
| TUI focus and invalidation | Expose proxy methods expected by TS extensions | Focus manager; invalidation/render scheduler | Focus transition and coalesced redraw tests |
| Overlay options/handle | Map percentages/anchors/callback visibility; retain TS handle identity | Overlay stack, layout constraints, focus/z-order lifecycle | QA fixture: anchors, stacking, responsive, hide/show, focus/unfocus |
| `setEditorComponent/get...` | Adapt pi `EditorComponent` and CustomEditor raw input contract | Replaceable editor slot with text/submit/change/history/paste ports | Modal editor fixture; wrapping prior editor; restore default |
| Header/footer/widgets | Adapt component factories, keyed identity, disposal, FooterDataProvider | Named slots and keyed persistent contributions | Replace/clear/dispose; placement; branch/status refresh |
| Status/working indicators | Preserve keyed status and pi reset/default semantics | Status model and activity indicator policy | Multiple keys, reset defaults, hidden/static/animated cases |
| Theme API | Provide pi token names and ANSI style functions to TS; invalidate cached ANSI | Semantic theme token map + theme generation/change event | Hot switch invalidates all components including cached content |
| KeybindingsManager / `matchesKey` | Preserve namespaced IDs and raw-sequence matching | Command IDs, configurable chord map, normalized dispatch | Remapped binding and conflict test; fullscreen precedence |
| `onTerminalInput` | Raw byte/ANSI subscription, consume/transform/unsubscribe | Optional pre-dispatch input middleware isolated from native input | consume, transform, unsubscribe, renderer switch rebind |
| `pasteToEditor` | Inject bracketed-paste behavior expected by pi extension | Typed paste command to active editor | Large/multiline paste fixture; no literal markers in native model |
| Clipboard/mouse/link behavior | OSC 52/OSC 8/SGR mouse, platform clipboard and image-paste glue | Clipboard and pointer capability ports | Capability-report tests plus fullscreen selection/link smoke test |
| Tool call/result renderers | Invoke TS renderer, preserve `state`/`lastComponent`, catch failures, parse ANSI component output | Tool row projection with call/result slots, partial/expanded/error state | Streaming partial, expanded, reuse, throw fallback, self-shell |
| Message/entry renderers | Invoke registered TS renderers and isolate failures | Custom transcript item renderer registry | Persist/restore custom entry; contextual custom message |
| Markdown transformers | Invoke sync TS chain with exact width/type/streaming context | Renderer-neutral Markdown transform stage | chain, failure continuation, resize and incomplete streaming Markdown |
| Main/fullscreen modes | ANSI screen sequences, TUI proxy compatibility | Renderer factory + stable frontend/controller reference | Swap renderer without session/core changes; preserve focus/components |
| Terminal capabilities | Detect Kitty, modifyOtherKeys, truecolor, mouse, OSC clipboard, IME cursor support | Versioned capability snapshot and change/downgrade reporting | Fake terminal matrix; unsupported feature returns/report, never silent |
| Shutdown/crash | Raw mode, cursor, keyboard protocol and alt-screen escape cleanup | Idempotent terminal session guard and shutdown coordinator | Normal/signal/panic/dead-terminal smoke tests |

## 9. Recommended boundary conclusions for e-agent design

1. **Renderer-neutral core UI contracts should be typed and semantic.** Session events/commands, reducer state, normalized input, focus/cursor anchors, overlays, slots, notifications, themes, command IDs, invalidation, and terminal capability data do not require ANSI or pi TypeScript types.
2. **The pi compatibility layer must be a dedicated adapter.** It alone should host TypeScript extension calls, raw terminal strings, ANSI/OSC parsing/rendering, `CURSOR_MARKER`, `matchesKey`, pi theme token names, promise/AbortSignal behavior, and pi component factories.
3. **Do not make every Rust component implement pi's string renderer.** Native Ratatui components should render to deterministic buffers. A compatibility component can parse pi-rendered ANSI lines into that buffer and report unsupported escapes/capabilities.
4. **Keep a stable frontend/controller reference while swapping concrete renderers.** Pi's proxy proves the interaction model; Rust can use an owned frontend runtime with a replaceable boxed renderer selected at the CLI composition root, without dynamic-library ABI.
5. **Treat capabilities as versioned negotiation.** At minimum report raw key releases, modified keys, mouse, clipboard text read/write/paste path, hyperlinks, color depth, hardware cursor/IME, main versus alternate screen, and pi adapter version. Unsupported edges return defaults/errors documented by capability rather than silently misbehaving.
6. **Compatibility needs real extension fixtures.** Import/adapt pinned examples for modal editor, custom header/footer/widgets, working indicator, tool renderer, Markdown/message/entry renderer, theme switching, autocomplete, and overlay QA. Rust-only mocks cannot establish “substantially unchanged” TypeScript compatibility.

## 10. Notable compatibility caveats

- The public compatibility target is broader than `ctx.ui`: custom tool, message, entry, and Markdown render registrations are required to reproduce documented extension UI.
- Overlay mode is labeled experimental in `extensions.md`, but it is explicitly required by the task PRD and has detailed public declarations and QA examples; pin behavior to 0.84.2 rather than promising future pi versions.
- Images are publicly available in pi-tui, but terminal image protocols are outside this task's stated compatibility target.
- RPC has UI but not TUI components. A single `hasUI` boolean is insufficient for capability negotiation; mode/capability checks must distinguish dialogs from component/raw-terminal support.
- `setHeader` is transcript/startup content in pi, while footer/widgets/editor are persistent composition slots. Preserve that distinction unless the e-agent design deliberately documents a different mapping.
- There is no documented arbitrary extension clipboard read/write API in 0.84.2. Do not invent one under “full ctx.ui compatibility”; expose capability-backed native clipboard ports separately if e-agent wants them.
