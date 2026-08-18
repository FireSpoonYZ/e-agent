use std::time::{Duration, Instant};

use e_agent_node_runtime::{HostcallKind, HostcallOutcome, HostcallRequest};
use e_agent_tui::{
    broker::{BrokerError, UiBrokerClient},
    ui_protocol::{
        Contribution, DialogRequest, DialogResult, ExtensionId, Generation, Notification,
        NotificationLevel, OverlayAction, OverlayId, SupportLevel, UiCapabilities, UiOperation,
        UiOperationKind, UiReply,
    },
};

pub const PI_UI_TARGET: &str = "0.84.2";

#[derive(Debug, Clone)]
pub struct PiUiConfig {
    mode: &'static str,
    broker: Option<UiBrokerClient>,
    capabilities: UiCapabilities,
}

impl Default for PiUiConfig {
    fn default() -> Self {
        Self::headless()
    }
}

impl PiUiConfig {
    pub fn headless() -> Self {
        Self {
            mode: "print",
            broker: None,
            capabilities: UiCapabilities::default(),
        }
    }

    pub fn interactive(broker: UiBrokerClient, capabilities: UiCapabilities) -> Self {
        Self {
            mode: "tui",
            broker: Some(broker),
            capabilities,
        }
    }

    pub fn mode(&self) -> &'static str {
        self.mode
    }

    pub fn has_ui(&self) -> bool {
        self.broker.is_some()
    }

    pub fn subscribe_input(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<e_agent_tui::input::InputEvent>> {
        self.broker.as_ref().map(UiBrokerClient::subscribe_input)
    }

    pub fn capabilities(&self) -> &UiCapabilities {
        &self.capabilities
    }

    pub async fn execute_hostcall(
        &self,
        extension_id: &str,
        op: &str,
        payload: serde_json::Value,
    ) -> Vec<HostcallOutcome> {
        self.execute(HostcallRequest {
            call_id: "pi-ui-adapter".into(),
            kind: HostcallKind::Ui { op: op.into() },
            payload,
            trace_id: 0,
            extension_id: Some(extension_id.into()),
        })
        .await
    }
    pub async fn execute(&self, request: HostcallRequest) -> Vec<HostcallOutcome> {
        let Some(operation) = operation(&request) else {
            return success(default_value(&request));
        };
        let default = default_value(&request);
        let Some(broker) = &self.broker else {
            return success(default);
        };
        if matches!(
            self.capabilities.support(operation.kind()),
            SupportLevel::Unsupported(_)
        ) {
            return success(default);
        }
        let extension = ExtensionId(
            request
                .extension_id
                .clone()
                .unwrap_or_else(|| "unknown".into()),
        );
        let deadline = request
            .payload
            .get("timeout")
            .and_then(serde_json::Value::as_u64)
            .map(|milliseconds| Instant::now() + Duration::from_millis(milliseconds));
        let reply = broker.request_until(extension, operation, deadline).await;
        success(match reply {
            Ok(reply) => reply_value(reply, default),
            Err(BrokerError::Closed | BrokerError::QueueFull | BrokerError::Busy) => default,
        })
    }
}

fn operation(request: &HostcallRequest) -> Option<UiOperation> {
    let payload = &request.payload;
    Some(match ui_op(request)? {
        "custom" if payload["mode"].as_str() == Some("poll") => UiOperation::TerminalInput {
            subscription: component_id(&text(payload, "widgetKey")),
            enabled: true,
        },
        "custom"
            if matches!(
                payload["mode"].as_str(),
                Some("hide" | "setHidden" | "focus" | "unfocus")
            ) =>
        {
            let action = match payload["mode"].as_str().unwrap_or_default() {
                "hide" => OverlayAction::Hide,
                "setHidden" => {
                    OverlayAction::SetHidden(payload["hidden"].as_bool().unwrap_or(false))
                }
                "focus" => OverlayAction::Focus,
                _ => OverlayAction::Unfocus,
            };
            UiOperation::Overlay {
                id: OverlayId(component_id(&text(payload, "widgetKey"))),
                generation: Generation(0),
                action,
            }
        }
        "select" => UiOperation::Dialog(DialogRequest::Select {
            title: text(payload, "title"),
            options: payload["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect(),
        }),
        "confirm" => UiOperation::Dialog(DialogRequest::Confirm {
            title: text(payload, "title"),
            message: text(payload, "message"),
        }),
        "input" => UiOperation::Dialog(DialogRequest::Input {
            title: text(payload, "title"),
            placeholder: text(payload, "placeholder"),
        }),
        "editor" => UiOperation::Dialog(DialogRequest::Editor {
            title: text(payload, "title"),
            prefill: text(payload, "default"),
        }),
        "notify" => UiOperation::Notify(Notification {
            message: text(payload, "message"),
            level: match payload
                .get("level")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("info")
            {
                "warning" => NotificationLevel::Warning,
                "error" => NotificationLevel::Error,
                _ => NotificationLevel::Info,
            },
        }),
        "setStatus" => contribution(payload, "status", "statusKey", "statusText"),
        "setFooter" => contribution_value("footer", "footer".into(), content(payload)),
        "setHeader" => contribution_value("header", "header".into(), content(payload)),
        "setWorkingMessage" => {
            contribution_value("working", "message".into(), text(payload, "message"))
        }
        "setWorkingVisible" => contribution_value(
            "working",
            "visible".into(),
            payload["visible"].as_bool().unwrap_or(true).to_string(),
        ),
        "setWorkingIndicator" => {
            contribution_value("working", "indicator".into(), content(payload))
        }
        "setHiddenThinkingLabel" => {
            contribution_value("working", "thinking-label".into(), text(payload, "label"))
        }
        "setWidget" => {
            let key = text(payload, "widgetKey");
            if key.starts_with("__pi_custom_") {
                let content = content(payload);
                if payload["overlay"].as_bool().unwrap_or(false) {
                    let id = OverlayId(component_id(&key));
                    UiOperation::Overlay {
                        id,
                        generation: Generation(0),
                        action: if content.is_empty() {
                            OverlayAction::Hide
                        } else {
                            OverlayAction::Show {
                                content,
                                capturing: !payload["nonCapturing"].as_bool().unwrap_or(false),
                            }
                        },
                    }
                } else {
                    UiOperation::CustomEditor {
                        content: (!payload["clear"].as_bool().unwrap_or(false)).then_some(content),
                    }
                }
            } else {
                let placement = payload["placement"].as_str().unwrap_or("aboveEditor");
                contribution_value(
                    if placement == "belowEditor" {
                        "below-widget"
                    } else {
                        "widget"
                    },
                    key,
                    content(payload),
                )
            }
        }
        "setTitle" => contribution_value("title", "title".into(), text(payload, "title")),
        "pasteToEditor" => UiOperation::Paste {
            text: text(payload, "text"),
        },
        "render" => {
            let key = text(payload, "key");
            let cached = crate::renderers::PiCachedFrame::from_ansi_lines(
                e_agent_tui::component::ComponentId(component_id(&key)),
                payload["lines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str),
                u16::try_from(payload["width"].as_u64().unwrap_or(80)).unwrap_or(80),
            );
            UiOperation::Frame {
                key,
                frame: cached.frame,
                cursor: cached.cursor,
            }
        }
        "setTheme" => UiOperation::Theme {
            generation: payload["generation"].as_u64().unwrap_or(0),
        },
        "setToolsExpanded" => contribution_value(
            "tools",
            "expanded".into(),
            payload["expanded"].as_bool().unwrap_or(false).to_string(),
        ),
        "setKeybindings" => UiOperation::Keybindings {
            entries: payload["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let pair = entry.as_array()?;
                    Some((
                        pair.first()?.as_str()?.to_owned(),
                        pair.get(1)?.as_str()?.to_owned(),
                    ))
                })
                .collect(),
        },
        "set_editor_text" | "setEditorText" => UiOperation::Editor {
            text: Some(text(payload, "text")),
        },
        "getEditorText" => UiOperation::Editor { text: None },
        _ => return None,
    })
}

fn ui_op(request: &HostcallRequest) -> Option<&str> {
    match &request.kind {
        HostcallKind::Ui { op } => Some(op),
        _ => None,
    }
}

fn contribution(payload: &serde_json::Value, slot: &str, key: &str, content: &str) -> UiOperation {
    contribution_value(slot, text(payload, key), text(payload, content))
}

fn contribution_value(slot: &str, key: String, content: String) -> UiOperation {
    UiOperation::Contribution(if content.is_empty() {
        Contribution::Remove {
            slot: slot.into(),
            key,
        }
    } else {
        Contribution::Set {
            slot: slot.into(),
            key,
            content,
        }
    })
}

fn component_id(key: &str) -> u64 {
    // Stable FNV-1a identity; only used to map a Pi component key to the
    // renderer-neutral overlay handle for this process.
    key.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn content(payload: &serde_json::Value) -> String {
    payload["lines"]
        .as_array()
        .map(|lines| {
            crate::ansi::plain_lines(lines.iter().filter_map(serde_json::Value::as_str), 4096)
        })
        .or_else(|| {
            payload["content"]
                .as_str()
                .map(|value| crate::ansi::plain_line(value, 4096))
        })
        .unwrap_or_default()
}
fn text(payload: &serde_json::Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn default_value(request: &HostcallRequest) -> serde_json::Value {
    match ui_op(request) {
        Some("confirm") => serde_json::Value::Bool(false),
        Some("getEditorText") => serde_json::Value::String(String::new()),
        _ => serde_json::Value::Null,
    }
}

fn reply_value(reply: UiReply, default: serde_json::Value) -> serde_json::Value {
    match reply {
        UiReply::Dialog(DialogResult::Selected(value))
        | UiReply::Dialog(DialogResult::Input(value))
        | UiReply::Dialog(DialogResult::Edited(value)) => {
            value.map_or(serde_json::Value::Null, serde_json::Value::String)
        }
        UiReply::Dialog(DialogResult::Confirmed(value)) => serde_json::Value::Bool(value),
        UiReply::Input(event) => pi_input_reply(event),
        UiReply::Value(value) => {
            serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value))
        }
        UiReply::Ack => serde_json::Value::Null,
        UiReply::Capabilities(_)
        | UiReply::Unsupported { .. }
        | UiReply::Busy
        | UiReply::Cancelled
        | UiReply::StaleHandle
        | UiReply::Failed(_) => default,
    }
}

fn pi_input_reply(event: e_agent_tui::input::InputEvent) -> serde_json::Value {
    if let e_agent_tui::input::InputEvent::Resize { columns, rows } = event {
        return serde_json::json!({"width": columns, "height": rows});
    }
    pi_input_data(&event)
        .map(|key| serde_json::json!({"key":key}))
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()))
}

pub(crate) fn pi_input_data(event: &e_agent_tui::input::InputEvent) -> Option<String> {
    use e_agent_tui::input::{InputEvent, KeyCode};

    Some(match event {
        InputEvent::Paste(text) => format!("\x1b[200~{text}\x1b[201~"),
        InputEvent::Text(text) => text.clone(),
        InputEvent::Key(key) => match key.code {
            KeyCode::Enter => "\r".to_owned(),
            KeyCode::Esc => "\x1b".to_owned(),
            KeyCode::Char(ch) if key.modifiers.ctrl => {
                char::from((ch.to_ascii_lowercase() as u8) & 0x1f).to_string()
            }
            KeyCode::Char(ch) => ch.to_string(),
            KeyCode::Backspace => "\x7f".to_owned(),
            KeyCode::Delete => "\x1b[3~".to_owned(),
            KeyCode::Left => "\x1b[D".to_owned(),
            KeyCode::Right => "\x1b[C".to_owned(),
            KeyCode::Up => "\x1b[A".to_owned(),
            KeyCode::Down => "\x1b[B".to_owned(),
            KeyCode::Home => "\x01".to_owned(),
            KeyCode::End => "\x05".to_owned(),
            KeyCode::PageUp => "\x1b[5~".to_owned(),
            KeyCode::PageDown => "\x1b[6~".to_owned(),
        },
        InputEvent::Mouse(_)
        | InputEvent::Resize { .. }
        | InputEvent::FocusGained
        | InputEvent::FocusLost => return None,
    })
}

fn success(value: serde_json::Value) -> Vec<HostcallOutcome> {
    vec![HostcallOutcome::Success(value)]
}

pub fn pi_operation_support(capabilities: &UiCapabilities, name: &str) -> SupportLevel {
    let kind = match name {
        "select" | "confirm" | "input" | "editor" => UiOperationKind::Dialog,
        "notify" => UiOperationKind::Notification,
        "setStatus"
        | "setWidget"
        | "setTitle"
        | "setHeader"
        | "setFooter"
        | "setWorkingMessage"
        | "setWorkingVisible"
        | "setWorkingIndicator"
        | "setHiddenThinkingLabel"
        | "setToolsExpanded" => UiOperationKind::Contribution,
        "setTheme" => UiOperationKind::Theme,
        "render" => UiOperationKind::Render,
        "setKeybindings" => UiOperationKind::Keybindings,
        "setEditorText" | "set_editor_text" | "getEditorText" | "pasteToEditor" => {
            UiOperationKind::Editor
        }
        "onTerminalInput" => UiOperationKind::TerminalInput,
        "custom" => UiOperationKind::Overlay,
        _ => UiOperationKind::Unknown,
    };
    capabilities.support(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use e_agent_node_runtime::HostcallKind;
    use e_agent_tui::{broker, ui_protocol::UiProtocolVersion};

    fn request(op: &str, payload: serde_json::Value) -> HostcallRequest {
        HostcallRequest {
            call_id: "call".into(),
            kind: HostcallKind::Ui { op: op.into() },
            payload,
            trace_id: 0,
            extension_id: Some("extension-a".into()),
        }
    }

    #[tokio::test]
    async fn interactive_broker_preserves_extension_payload_and_reply() {
        let mut capabilities = UiCapabilities::default();
        capabilities
            .operations
            .insert(UiOperationKind::Dialog, SupportLevel::Adapted);
        let (client, mut server) = broker::channel(capabilities.clone());
        let ui = PiUiConfig::interactive(client, capabilities);
        let task = tokio::spawn(async move {
            ui.execute(request(
                "select",
                serde_json::json!({"title":"Choose", "options":["a", "b"]}),
            ))
            .await
        });
        let envelope = server.recv().await.unwrap();
        assert_eq!(envelope.version, UiProtocolVersion::CURRENT);
        assert_eq!(envelope.extension, ExtensionId("extension-a".into()));
        assert_eq!(
            envelope.operation,
            UiOperation::Dialog(DialogRequest::Select {
                title: "Choose".into(),
                options: vec!["a".into(), "b".into()],
            })
        );
        server.reply(
            envelope.request,
            UiReply::Dialog(DialogResult::Selected(Some("b".into()))),
        );
        assert!(matches!(
            task.await.unwrap()[0],
            HostcallOutcome::Success(serde_json::Value::String(ref value)) if value == "b"
        ));
    }

    #[tokio::test]
    async fn fire_and_forget_contributions_keep_order_and_payload() {
        let mut capabilities = UiCapabilities::default();
        capabilities
            .operations
            .insert(UiOperationKind::Contribution, SupportLevel::Adapted);
        let (client, mut server) = broker::channel(capabilities.clone());
        let ui = PiUiConfig::interactive(client, capabilities);
        let first = tokio::spawn({
            let ui = ui.clone();
            async move {
                ui.execute(request(
                    "setStatus",
                    serde_json::json!({"statusKey":"build", "statusText":"one"}),
                ))
                .await
            }
        });
        let one = server.recv().await.unwrap();
        assert_eq!(one.extension, ExtensionId("extension-a".into()));
        assert_eq!(
            one.operation,
            UiOperation::Contribution(Contribution::Set {
                slot: "status".into(),
                key: "build".into(),
                content: "one".into(),
            })
        );
        server.reply(one.request, UiReply::Ack);
        first.await.unwrap();

        let second = tokio::spawn(async move {
            ui.execute(request(
                "setStatus",
                serde_json::json!({"statusKey":"build", "statusText":"two"}),
            ))
            .await
        });
        let two = server.recv().await.unwrap();
        assert!(
            matches!(two.operation, UiOperation::Contribution(Contribution::Set { content, .. }) if content == "two")
        );
        server.reply(two.request, UiReply::Ack);
        second.await.unwrap();
    }

    #[tokio::test]
    async fn timeout_and_shutdown_settle_to_defaults() {
        let mut capabilities = UiCapabilities::default();
        capabilities
            .operations
            .insert(UiOperationKind::Dialog, SupportLevel::Adapted);
        let (client, mut server) = broker::channel(capabilities.clone());
        let ui = PiUiConfig::interactive(client, capabilities);
        let timed = tokio::spawn({
            let ui = ui.clone();
            async move {
                ui.execute(request("confirm", serde_json::json!({"timeout": 1})))
                    .await
            }
        });
        let _ = server.recv().await.unwrap();
        assert!(matches!(
            timed.await.unwrap()[0],
            HostcallOutcome::Success(serde_json::Value::Bool(false))
        ));
        let pending =
            tokio::spawn(async move { ui.execute(request("input", serde_json::json!({}))).await });
        let _ = server.recv().await.unwrap();
        drop(server);
        assert!(matches!(
            pending.await.unwrap()[0],
            HostcallOutcome::Success(serde_json::Value::Null)
        ));
    }

    #[tokio::test]
    async fn adapter_maps_paste_slots_and_explicit_capabilities() {
        let mut capabilities = UiCapabilities::default();
        for kind in [
            UiOperationKind::Editor,
            UiOperationKind::Contribution,
            UiOperationKind::Theme,
            UiOperationKind::Keybindings,
        ] {
            capabilities.operations.insert(kind, SupportLevel::Adapted);
        }
        let (client, mut server) = broker::channel(capabilities.clone());
        let ui = PiUiConfig::interactive(client, capabilities);

        let paste = tokio::spawn({
            let ui = ui.clone();
            async move {
                ui.execute(request("pasteToEditor", serde_json::json!({"text":"x\ny"})))
                    .await
            }
        });
        let envelope = server.recv().await.unwrap();
        assert_eq!(
            envelope.operation,
            UiOperation::Paste {
                text: "x\ny".into()
            }
        );
        server.reply(envelope.request, UiReply::Ack);
        assert!(matches!(
            paste.await.unwrap()[0],
            HostcallOutcome::Success(serde_json::Value::Null)
        ));

        let footer = tokio::spawn(async move {
            ui.execute(request(
                "setFooter",
                serde_json::json!({"content":"\u{1b}[31mfooter\u{1b}[0m"}),
            ))
            .await
        });
        let envelope = server.recv().await.unwrap();
        assert_eq!(
            envelope.operation,
            UiOperation::Contribution(Contribution::Set {
                slot: "footer".into(),
                key: "footer".into(),
                content: "footer".into(),
            })
        );
        server.reply(envelope.request, UiReply::Ack);
        footer.await.unwrap();
        assert!(matches!(
            pi_operation_support(server.capabilities(), "onTerminalInput"),
            SupportLevel::Unsupported(_)
        ));
    }
    #[test]
    fn custom_widget_frames_use_overlay_or_editor_operations() {
        let overlay = operation(&request(
            "setWidget",
            serde_json::json!({
                "widgetKey":"__pi_custom_7",
                "overlay":true,
                "lines":["one", "two"],
            }),
        ))
        .expect("map custom overlay");
        assert!(matches!(
            overlay,
            UiOperation::Overlay {
                action: OverlayAction::Show { content, capturing: true },
                ..
            } if content == "one\ntwo"
        ));

        let editor = operation(&request(
            "setWidget",
            serde_json::json!({"widgetKey":"__pi_custom_7", "lines":["editor"]}),
        ))
        .expect("map custom editor");
        assert_eq!(
            editor,
            UiOperation::CustomEditor {
                content: Some("editor".into())
            }
        );

        let close = operation(&request(
            "setWidget",
            serde_json::json!({"widgetKey":"__pi_custom_7", "overlay":true, "lines":[]}),
        ))
        .expect("map custom close");
        assert!(matches!(
            close,
            UiOperation::Overlay {
                action: OverlayAction::Hide,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn custom_input_poll_returns_adapter_encoded_native_input() {
        let mut capabilities = UiCapabilities::default();
        capabilities
            .operations
            .insert(UiOperationKind::TerminalInput, SupportLevel::Adapted);
        let (client, mut server) = broker::channel(capabilities.clone());
        let ui = PiUiConfig::interactive(client, capabilities);
        let task = tokio::spawn(async move {
            ui.execute(request(
                "custom",
                serde_json::json!({"mode":"poll", "widgetKey":"__pi_custom_1"}),
            ))
            .await
        });
        let envelope = server.recv().await.unwrap();
        assert!(matches!(
            envelope.operation,
            UiOperation::TerminalInput {
                subscription: _,
                enabled: true
            }
        ));
        assert!(server.queue_input_poll(envelope.request));
        assert!(server.reply_input(e_agent_tui::input::InputEvent::Key(
            e_agent_tui::input::KeyEvent {
                code: e_agent_tui::input::KeyCode::Char('a'),
                modifiers: e_agent_tui::input::Modifiers::default(),
                kind: e_agent_tui::input::KeyKind::Press,
            },
        )));
        assert!(matches!(
            task.await.unwrap()[0],
            HostcallOutcome::Success(serde_json::Value::Object(ref value))
                if value["key"] == "a"
        ));
    }

    #[tokio::test]
    async fn headless_dialogs_settle_to_pi_defaults() {
        let ui = PiUiConfig::headless();
        assert!(matches!(
            ui.execute(request("select", serde_json::json!({}))).await[0],
            HostcallOutcome::Success(serde_json::Value::Null)
        ));
        assert!(matches!(
            ui.execute(request("confirm", serde_json::json!({}))).await[0],
            HostcallOutcome::Success(serde_json::Value::Bool(false))
        ));
        assert!(
            matches!(ui.execute(request("getEditorText", serde_json::json!({}))).await[0], HostcallOutcome::Success(serde_json::Value::String(ref value)) if value.is_empty())
        );
    }
}
