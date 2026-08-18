# Stage 6 Progress - Pi 0.84.2 Compatibility Adapter

## Status

Implementation complete. Final verification was explicitly deferred by the user before commit.

The last attempted final gate was aborted before completion. Do not treat the current commit as having passed the complete Stage 6 validation matrix. Resume by running the commands in **Pending verification**.

## Completed implementation

- Pinned Pi `0.84.2` fixture provenance with SHA-256 integrity checks.
- Explicit interactive/headless Pi UI configuration and operation-level capabilities.
- Dialog, notification, status, working indicator, widget, header, footer, title, editor text, paste, theme, and keybinding broker translation.
- `ctx.ui.custom()` async factories, idempotent completion, disposal isolation, focused input, overlay/editor mapping, and overlay handles (`hide`, `setHidden`, `focus`, `unfocus`).
- Non-blocking broker input broadcast for persistent custom editors.
- Registration-ordered `onTerminalInput` observe/transform/consume/unsubscribe behavior.
- Stateful Pi `CustomEditor` compatibility shim and unchanged `modal-editor.ts` fixture mounting/input path.
- Synchronous theme discovery/get/set contract and ANSI styling helpers.
- Editor factory get/set lifecycle and autocomplete factory composition storage.
- Pi-registered tools exposed as first-class agent tools.
- Tool `renderCall` / `renderResult` execution on the JS runtime thread with row-local `state`, `lastComponent`, partial/final flags, and native fallback.
- Custom message and persisted entry renderer invocation with per-item failure isolation.
- Markdown transformer chain with message type, streaming, available width, resize context, and failure continuation.
- ANSI/OSC adapter for CJK width, clipping, SGR foreground/background and modifiers, OSC 8 hyperlinks, unsafe control sanitization, and Pi cursor markers.
- Immutable semantic frames and cursor anchors crossing the generic UI protocol; Ratatui consumes semantic styles without invoking JavaScript during render.
- Five-second default UI hostcall deadline; late completions are ignored by existing hostcall correlation.
- Terminal images and undocumented Pi internals remain explicitly unsupported.

## Compatibility fixtures

Stored under `e-agent-pi-compat/tests/fixtures/pi-0.84.2/`:

- overlay QA
- custom header/footer
- modal editor
- widget placement
- working indicator
- autocomplete
- tool renderer
- message renderer
- entry renderer
- system theme
- Markdown transformer
- terminal-input subscription

The widget placement and modal editor fixtures have end-to-end runtime/broker tests without source rewriting. Other fixtures are pinned by integrity checks and are scheduled for the Stage 9 unchanged-fixture acceptance suite.

## Verification completed before the final input-broadcast changes

These checks passed during Stage 6 development:

- `cargo test -p e-agent-pi-compat` (20 unit/integration tests plus fixture integrity)
- `cargo test -p e-agent-tui` (25 tests)
- targeted node-runtime tests for source receipts, custom UI reflow, Markdown transforms, terminal input, themes/editors/autocomplete, tool rendering, and hostcall timeout
- `cargo check --workspace`
- `cargo fmt --all -- --check`
- `git diff --check`
- LSP diagnostics for changed Rust packages

After adding persistent editor input broadcast, only targeted compile and unchanged modal-editor input tests were run successfully. The complete final gate was then stopped at the user's request.

## Known full-suite baseline

A full `e-agent-node-runtime --features runtime-tests --lib` run completed 445 of 452 tests. Seven failures were outside the Stage 6 UI paths:

- two missing Doom conformance artifacts
- module/path candidate and monorepo escape fixtures
- cached VFS/symlink scope fixture
- cross-extension host-write fixture
- critical-command exec mediation fixture

Do not silently classify these as Stage 6 regressions or successes. Reproduce and triage them separately if the task requires a fully green workspace-wide runtime suite.

## Pending verification

```bash
cargo fmt --all -- --check
cargo test -p e-agent-node-runtime --features runtime-tests --lib compressed_javascript_sources_preserve_exact_bytes
cargo test -p e-agent-node-runtime --features runtime-tests --lib pijs_custom_ui_width_updates_trigger_reflow
cargo test -p e-agent-node-runtime --features runtime-tests --lib pijs_markdown_transformers_continue_after_failure
cargo test -p e-agent-node-runtime --features runtime-tests --lib pijs_terminal_input_listeners_transform_consume_and_unsubscribe
cargo test -p e-agent-node-runtime --features runtime-tests --lib pijs_theme_editor_and_autocomplete_contracts_are_synchronous_and_stable
cargo test -p e-agent-node-runtime --features runtime-tests --lib pijs_tool_component_producer_retains_row_state_and_falls_back_when_absent
cargo test -p e-agent-node-runtime --features runtime-tests --lib pijs_hostcall_timeout_rejects_promise
cargo test -p e-agent-pi-compat
cargo test -p e-agent-tui
cargo check --workspace
git diff --check
```

The complete node-runtime feature suite may still report the seven baseline failures listed above.

## Next stage

Stage 7 splits renderer-neutral contracts into `e-agent-tui-api`, moves Ratatui/Crossterm into `e-agent-tui-ratatui`, retains `e-agent-tui` as the legacy facade, and exposes compile-time frontend selection in the CLI.
