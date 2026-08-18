use std::collections::BTreeMap;

use e_agent_core::{AgentEvent, Message, MessageDelta, SessionStatus};

use crate::{
    input::{CommandId, InputEvent, KeyCode, KeyKind, MouseKind},
    state::{AppState, ToolState, backspace, delete, insert_text, move_vertical},
    ui_protocol::{
        Contribution, DialogRequest, DialogResult, OverlayAction, UiEnvelope, UiOperation, UiReply,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCommand {
    Prompt(String),
    Steer(String),
    FollowUp(String),
    Abort,
    Close,
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    Session(AgentEvent),
    Input(InputEvent),
    Ui(UiEnvelope),
    ObserverLagged(u64),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Session(SessionCommand),
    UiReply(crate::ui_protocol::RequestId, UiReply),
    SetTitle(String),
    Exit,
}

pub fn reduce(state: &mut AppState, event: AppEvent) -> Vec<Effect> {
    match event {
        AppEvent::Session(event) => reduce_session(state, event),
        AppEvent::Input(event) => return reduce_input(state, event),
        AppEvent::Ui(envelope) => return reduce_ui(state, envelope),
        AppEvent::ObserverLagged(skipped) => {
            state.status = SessionStatus::Fatal;
            state.fatal_error = Some(format!("event observer lagged by {skipped} records"));
        }
        AppEvent::Shutdown => {
            return vec![
                Effect::Session(SessionCommand::Abort),
                Effect::Session(SessionCommand::Close),
            ];
        }
    }
    Vec::new()
}

fn reduce_session(state: &mut AppState, event: AgentEvent) {
    match event {
        AgentEvent::AgentStart { .. } => state.status = SessionStatus::Running,
        AgentEvent::MessageStart {
            message_id,
            message: Message::Assistant(_),
        } => {
            state.partial_id = Some(message_id);
            state.partial.clear();
            state.thinking.clear();
            state.tool_call_input.clear();
            state.partial_unpersisted = false;
        }
        AgentEvent::MessageUpdate {
            message_id,
            delta: MessageDelta::Thinking(text),
            ..
        } if state.partial_id.as_deref() == Some(&message_id) => state.thinking.push_str(&text),
        AgentEvent::MessageUpdate {
            message_id,
            delta: MessageDelta::ToolCallInput(input),
            ..
        } if state.partial_id.as_deref() == Some(&message_id) => {
            state.tool_call_input.push_str(&input)
        }
        AgentEvent::MessageUpdate {
            message_id,
            delta: MessageDelta::Text(text),
            ..
        } if state.partial_id.as_deref() == Some(&message_id) => state.partial.push_str(&text),
        AgentEvent::MessageEnd {
            message_id,
            message,
        } => {
            if state.partial_id.as_deref() == Some(&message_id) {
                state.partial_id = None;
                state.partial.clear();
                state.thinking.clear();
                state.tool_call_input.clear();
                state.partial_unpersisted = false;
            }
            state.messages.push(message);
        }
        AgentEvent::ToolExecutionStart { id, name, input } => {
            state.tools.insert(
                id,
                ToolState {
                    name,
                    status: "running".into(),
                    input,
                    update: None,
                    result: None,
                    is_error: false,
                },
            );
        }
        AgentEvent::ToolExecutionUpdate { id, update } => {
            if let Some(tool) = state.tools.get_mut(&id) {
                tool.status = "updating".into();
                tool.update = Some(update);
            }
        }
        AgentEvent::ToolExecutionEnd {
            id,
            result,
            is_error,
            ..
        } => {
            if let Some(tool) = state.tools.get_mut(&id) {
                tool.status = if is_error { "error" } else { "done" }.into();
                tool.result = Some(result);
                tool.is_error = is_error;
            }
        }
        AgentEvent::AgentSettled { .. } => state.status = SessionStatus::Idle,
        AgentEvent::SessionFatal { error } => {
            state.status = SessionStatus::Fatal;
            state.partial_unpersisted = !state.partial.is_empty();
            state.fatal_error = Some(error);
            for tool in state.tools.values_mut() {
                if matches!(tool.status.as_str(), "running" | "updating") {
                    tool.status = "interrupted".into();
                }
            }
        }
        AgentEvent::SessionShutdown if state.status != SessionStatus::Fatal => {
            state.status = SessionStatus::Closed
        }
        _ => {}
    }
}

fn reduce_ui(state: &mut AppState, envelope: UiEnvelope) -> Vec<Effect> {
    let request = envelope.request;
    let extension = envelope.extension;
    let reply = match envelope.operation {
        UiOperation::Dialog(dialog) => {
            let text = match &dialog {
                DialogRequest::Editor { prefill, .. } => prefill.clone(),
                _ => String::new(),
            };
            state.ui.dialog = Some(crate::state::PendingDialog {
                request,
                cursor: text.chars().count(),
                text,
                selected: 0,
                dialog,
            });
            return Vec::new();
        }
        UiOperation::Notify(notification) => {
            state.ui.notifications.push_back(notification);
            while state.ui.notifications.len() > 32 {
                state.ui.notifications.pop_front();
            }
            UiReply::Ack
        }
        UiOperation::Contribution(contribution) => {
            match contribution {
                Contribution::Set { slot, key, content } if slot == "title" => {
                    state
                        .ui
                        .contributions
                        .insert((extension, slot, key), content.clone());
                    return vec![
                        Effect::SetTitle(content),
                        Effect::UiReply(request, UiReply::Ack),
                    ];
                }
                Contribution::Set { slot, key, content } => {
                    state
                        .ui
                        .contributions
                        .insert((extension, slot, key), content);
                }
                Contribution::Remove { slot, key } => {
                    state.ui.contributions.remove(&(extension, slot, key));
                }
            }
            UiReply::Ack
        }
        UiOperation::Editor { text: Some(text) } => {
            state.editor = text;
            state.cursor = state.editor.chars().count();
            UiReply::Ack
        }
        UiOperation::Editor { text: None } => UiReply::Value(state.editor.clone()),
        UiOperation::Paste { text } => {
            state.insert_text(&text);
            UiReply::Ack
        }
        UiOperation::CustomEditor { content } => {
            state.ui.custom_editor =
                content.map(|content| (extension, state.editor.clone(), content));
            UiReply::Ack
        }
        UiOperation::Overlay {
            id,
            generation,
            action,
        } => {
            let index = state
                .ui
                .overlays
                .iter()
                .position(|overlay| overlay.id == id && overlay.generation == generation);
            match action {
                OverlayAction::Show { content, capturing } => {
                    state.ui.overlays.push(crate::state::OverlayState {
                        extension,
                        id,
                        generation,
                        content,
                        hidden: false,
                        capturing,
                    });
                }
                OverlayAction::Hide => {
                    let Some(index) = index else {
                        return vec![Effect::UiReply(request, UiReply::StaleHandle)];
                    };
                    state.ui.overlays.remove(index);
                }
                OverlayAction::SetHidden(hidden) => {
                    let Some(index) = index else {
                        return vec![Effect::UiReply(request, UiReply::StaleHandle)];
                    };
                    state.ui.overlays[index].hidden = hidden;
                }
                OverlayAction::Focus => {
                    let Some(index) = index else {
                        return vec![Effect::UiReply(request, UiReply::StaleHandle)];
                    };
                    let overlay = state.ui.overlays.remove(index);
                    state.ui.overlays.push(overlay);
                }
                OverlayAction::Unfocus => {
                    let Some(index) = index else {
                        return vec![Effect::UiReply(request, UiReply::StaleHandle)];
                    };
                    if index > 0 {
                        state.ui.overlays.swap(index, index - 1);
                    }
                }
            }
            UiReply::Ack
        }
        UiOperation::Theme { generation } => {
            state.ui.theme_generation = generation;
            UiReply::Ack
        }
        UiOperation::Keybindings { entries } => {
            state.ui.keybindings.clear();
            state.ui.keybinding_conflicts.clear();
            let mut seen = BTreeMap::<String, String>::new();
            for (command, key) in entries {
                if let Some(previous) = seen.insert(key.clone(), command.clone()) {
                    state
                        .ui
                        .keybinding_conflicts
                        .push(format!("{key}: {previous}, {command}"));
                }
                state.ui.keybindings.insert(key, CommandId(command));
            }
            UiReply::Ack
        }
        UiOperation::Render { key, content } => {
            state
                .ui
                .contributions
                .insert((extension, "render".into(), key), content);
            UiReply::Ack
        }
        UiOperation::Frame { key, frame, cursor } => {
            state.ui.frames.insert((extension, key), (frame, cursor));
            UiReply::Ack
        }
        UiOperation::Capabilities
        | UiOperation::TerminalInput { .. }
        | UiOperation::Clipboard { .. }
        | UiOperation::Unknown(_) => return Vec::new(),
    };
    vec![Effect::UiReply(request, reply)]
}

fn reduce_input(state: &mut AppState, event: InputEvent) -> Vec<Effect> {
    if let Some(dialog) = state.ui.dialog.as_mut() {
        let result = handle_dialog(dialog, &event);
        if let Some(result) = result {
            let request = dialog.request;
            state.ui.dialog = None;
            return vec![Effect::UiReply(request, UiReply::Dialog(result))];
        }
        return Vec::new();
    }
    if state
        .ui
        .overlays
        .iter()
        .rev()
        .any(|overlay| !overlay.hidden && overlay.capturing)
    {
        if matches!(
            event,
            InputEvent::Key(crate::input::KeyEvent {
                code: KeyCode::Esc,
                kind: KeyKind::Press | KeyKind::Repeat,
                ..
            })
        ) {
            if let Some(index) = state
                .ui
                .overlays
                .iter()
                .rposition(|overlay| !overlay.hidden && overlay.capturing)
            {
                state.ui.overlays.remove(index);
            }
        }
        return Vec::new();
    }
    if let Some(command) = state.resolve_command(&event).cloned() {
        return command_effect(state, &command);
    }
    if let InputEvent::Mouse(event) = event {
        match event.kind {
            MouseKind::ScrollUp => state.scroll_up(3),
            MouseKind::ScrollDown => state.scroll_down(3),
            _ => {}
        }
        return Vec::new();
    }
    if let InputEvent::Resize { columns, rows } = event {
        state.ui.terminal_size = (columns, rows);
        return Vec::new();
    }
    if matches!(event, InputEvent::FocusGained | InputEvent::FocusLost) {
        state.ui.terminal_focused = matches!(event, InputEvent::FocusGained);
        return Vec::new();
    }
    if let InputEvent::Paste(text) | InputEvent::Text(text) = event {
        if state.status != SessionStatus::Fatal {
            state.insert_text(&text);
        }
        return Vec::new();
    }
    let InputEvent::Key(event) = event else {
        return Vec::new();
    };
    if !matches!(event.kind, KeyKind::Press | KeyKind::Repeat) {
        return Vec::new();
    }
    if event.code == KeyCode::Esc {
        return vec![Effect::Exit];
    }
    if event.code == KeyCode::Char('c') && event.modifiers.ctrl {
        return if state.status == SessionStatus::Running {
            vec![Effect::Session(SessionCommand::Abort)]
        } else {
            vec![Effect::Exit]
        };
    }
    if state.status == SessionStatus::Fatal {
        return Vec::new();
    }
    match event.code {
        KeyCode::Enter if event.modifiers.shift => state.insert('\n'),
        KeyCode::Enter => {
            if let Some(text) = state.take_editor() {
                return vec![Effect::Session(SessionCommand::Prompt(text))];
            }
        }
        KeyCode::Char(ch) if !event.modifiers.ctrl => state.insert(ch),
        KeyCode::Backspace => state.backspace(),
        KeyCode::Delete => state.delete(),
        KeyCode::Left => state.cursor = state.cursor.saturating_sub(1),
        KeyCode::Right => state.cursor = (state.cursor + 1).min(state.editor.chars().count()),
        KeyCode::Home => state.cursor = 0,
        KeyCode::End => state.cursor = state.editor.chars().count(),
        KeyCode::Up if state.editor.is_empty() => state.scroll_up(1),
        KeyCode::Down if state.editor.is_empty() => state.scroll_down(1),
        KeyCode::Up => state.move_vertical(-1),
        KeyCode::Down => state.move_vertical(1),
        KeyCode::PageUp => state.scroll_up(10),
        KeyCode::PageDown => state.scroll_down(10),
        _ => {}
    }
    Vec::new()
}

fn command_effect(state: &mut AppState, command: &CommandId) -> Vec<Effect> {
    match command.0.as_str() {
        "app.exit" => vec![Effect::Exit],
        "app.interrupt" if state.status == SessionStatus::Running => {
            vec![Effect::Session(SessionCommand::Abort)]
        }
        "input.newline" => {
            state.insert('\n');
            Vec::new()
        }
        "transcript.page-up" => {
            state.scroll_up(10);
            Vec::new()
        }
        "transcript.page-down" => {
            state.scroll_down(10);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn handle_dialog(
    dialog: &mut crate::state::PendingDialog,
    event: &InputEvent,
) -> Option<DialogResult> {
    let InputEvent::Key(key) = event else {
        if let InputEvent::Paste(text) | InputEvent::Text(text) = event {
            insert_text(&mut dialog.text, &mut dialog.cursor, text);
        }
        return None;
    };
    if !matches!(key.kind, KeyKind::Press | KeyKind::Repeat) {
        return None;
    }
    if key.code == KeyCode::Esc {
        return Some(cancelled_dialog(&dialog.dialog));
    }
    match &dialog.dialog {
        DialogRequest::Select { options, .. } => match key.code {
            KeyCode::Up => dialog.selected = dialog.selected.saturating_sub(1),
            KeyCode::Down => {
                dialog.selected = (dialog.selected + 1).min(options.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                return Some(DialogResult::Selected(
                    options.get(dialog.selected).cloned(),
                ));
            }
            _ => {}
        },
        DialogRequest::Confirm { .. } => match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                return Some(DialogResult::Confirmed(true));
            }
            KeyCode::Char('n' | 'N') => return Some(DialogResult::Confirmed(false)),
            _ => {}
        },
        DialogRequest::Input { .. } | DialogRequest::Editor { .. } => match key.code {
            KeyCode::Enter
                if matches!(dialog.dialog, DialogRequest::Editor { .. }) && key.modifiers.shift =>
            {
                insert_text(&mut dialog.text, &mut dialog.cursor, "\n")
            }
            KeyCode::Enter => {
                return Some(match dialog.dialog {
                    DialogRequest::Input { .. } => DialogResult::Input(Some(dialog.text.clone())),
                    _ => DialogResult::Edited(Some(dialog.text.clone())),
                });
            }
            KeyCode::Char(ch) if !key.modifiers.ctrl => {
                insert_text(&mut dialog.text, &mut dialog.cursor, &ch.to_string())
            }
            KeyCode::Backspace => backspace(&mut dialog.text, &mut dialog.cursor),
            KeyCode::Delete => delete(&mut dialog.text, dialog.cursor),
            KeyCode::Left => dialog.cursor = dialog.cursor.saturating_sub(1),
            KeyCode::Right => dialog.cursor = (dialog.cursor + 1).min(dialog.text.chars().count()),
            KeyCode::Up => move_vertical(&dialog.text, &mut dialog.cursor, -1),
            KeyCode::Down => move_vertical(&dialog.text, &mut dialog.cursor, 1),
            _ => {}
        },
    }
    None
}

fn cancelled_dialog(dialog: &DialogRequest) -> DialogResult {
    match dialog {
        DialogRequest::Select { .. } => DialogResult::Selected(None),
        DialogRequest::Confirm { .. } => DialogResult::Confirmed(false),
        DialogRequest::Input { .. } => DialogResult::Input(None),
        DialogRequest::Editor { .. } => DialogResult::Edited(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        input::{KeyEvent, Modifiers},
        ui_protocol::{ExtensionId, Generation, RequestId, UiProtocolVersion},
    };

    fn key(code: KeyCode) -> InputEvent {
        InputEvent::Key(KeyEvent {
            code,
            modifiers: Modifiers::default(),
            kind: KeyKind::Press,
        })
    }

    #[test]
    fn input_reduces_to_prompt_without_terminal() {
        let mut state = AppState::new(Vec::new(), SessionStatus::Idle);
        reduce(
            &mut state,
            AppEvent::Input(InputEvent::Text("hello".into())),
        );
        let effects = reduce(&mut state, AppEvent::Input(key(KeyCode::Enter)));
        assert_eq!(
            effects,
            vec![Effect::Session(SessionCommand::Prompt("hello".into()))]
        );
    }

    #[test]
    fn dialog_consumes_escape_before_application_exit() {
        let mut state = AppState::new(Vec::new(), SessionStatus::Idle);
        reduce(
            &mut state,
            AppEvent::Ui(UiEnvelope {
                version: UiProtocolVersion::CURRENT,
                extension: ExtensionId("x".into()),
                request: RequestId(1),
                generation: Generation(0),
                deadline: None,
                operation: UiOperation::Dialog(DialogRequest::Confirm {
                    title: "t".into(),
                    message: "m".into(),
                }),
            }),
        );
        assert_eq!(
            reduce(&mut state, AppEvent::Input(key(KeyCode::Esc))),
            vec![Effect::UiReply(
                RequestId(1),
                UiReply::Dialog(DialogResult::Confirmed(false))
            )]
        );
    }

    #[test]
    fn keybindings_report_conflicts_and_override_fallback() {
        let mut state = AppState::new(Vec::new(), SessionStatus::Idle);
        reduce(
            &mut state,
            AppEvent::Ui(UiEnvelope {
                version: UiProtocolVersion::CURRENT,
                extension: ExtensionId("x".into()),
                request: RequestId(1),
                generation: Generation(0),
                deadline: None,
                operation: UiOperation::Keybindings {
                    entries: vec![
                        ("input.newline".into(), "esc".into()),
                        ("app.exit".into(), "esc".into()),
                    ],
                },
            }),
        );
        assert_eq!(state.ui.keybinding_conflicts.len(), 1);
        assert_eq!(
            reduce(&mut state, AppEvent::Input(key(KeyCode::Esc))),
            vec![Effect::Exit]
        );
    }
}
