use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One entry in the conversation history.
///
/// The role is the variant rather than a field so that role-specific data has a
/// home: only assistant messages carry `stop_reason`, and only tool results
/// carry a `tool_use_id`. It also makes a tool result nested inside a tool
/// result unrepresentable, which a single `{ role, content }` shape allowed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    /// The host's answer to one `MessageContent::ToolUse`.
    ///
    /// Providers place this at the top level of a request (OpenAI calls it
    /// `function_call_output`), not inside a user message.
    ToolResult(ToolResultMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub content: Vec<MessageContent>,
}

impl UserMessage {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![MessageContent::text(text)],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub content: Vec<MessageContent>,
    /// Why the model stopped. The agent loop needs this to tell "wants a tool"
    /// from "done" from "truncated" instead of inferring it from the content.
    pub stop_reason: StopReason,
    /// Absent when the provider does not report token counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    /// Matches the `MessageContent::ToolUse::id` this answers. Providers pair the
    /// call and its result by this value.
    pub tool_use_id: String,
    pub content: Vec<MessageContent>,
    /// The tool ran but reported failure (a non-zero exit, a raised exception).
    /// The content is still meaningful and goes to the model either way.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    /// Whether this answers an OpenAI custom tool call rather than a function call.
    #[serde(default, skip_serializing_if = "is_false")]
    pub custom: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde requires `fn(&bool) -> bool` here
const fn is_false(value: &bool) -> bool {
    !*value
}

/// Why a response ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its turn.
    #[default]
    Stop,
    /// The model wants tools executed and the loop to continue.
    ToolUse,
    /// Output hit a token limit; the content is truncated, not complete.
    Length,
    /// The model declined to answer.
    Refusal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Everything a single completion request needs, independent of the backend.
///
/// `system_prompt` stays out of `messages` on purpose: the Responses API takes
/// it as the top-level `instructions` field, and some endpoints reject system
/// messages inside the input array.
#[derive(Debug, Default, Clone)]
pub struct Context<'a> {
    pub system_prompt: Option<&'a str>,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDef],
}

/// A tool advertised to the model.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input: ToolInput,
}

#[derive(Debug, Clone)]
pub enum ToolInput {
    Json(Value),
    Text,
    Lark(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
    Text {
        text: String,
    },
    /// Model reasoning. `signature` is the opaque token some providers require
    /// to replay the block on a later turn; without one the block is display-only.
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// A tool call requested by the model. Answered by a `Message::ToolResult`
    /// carrying the same `id`.
    ToolUse {
        id: String,
        name: String,
        input: String,
        custom: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
    },
}

impl Message {
    pub fn content(&self) -> &[MessageContent] {
        match self {
            Self::User(message) => &message.content,
            Self::Assistant(message) => &message.content,
            Self::ToolResult(message) => &message.content,
        }
    }

    /// The tool calls this message asks the host to run.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.content().iter().filter_map(|block| match block {
            MessageContent::ToolUse {
                id, name, input, ..
            } => Some((id.as_str(), name.as_str(), input.as_str())),
            _ => None,
        })
    }
}

impl ToolResultMessage {
    /// A successful text result.
    pub fn text(tool_use_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: vec![MessageContent::Text { text: text.into() }],
            is_error: false,
            custom: false,
        }
    }

    /// A failed result. The message is what the model sees, so it has to explain
    /// the failure well enough to retry from.
    pub fn error(tool_use_id: impl Into<String>, message: impl std::fmt::Display) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: vec![MessageContent::Text {
                text: message.to_string(),
            }],
            is_error: true,
            custom: false,
        }
    }
}

impl MessageContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_tool_call_and_its_result() {
        let history = vec![
            Message::User(UserMessage::text("run it")),
            Message::Assistant(AssistantMessage {
                content: vec![
                    MessageContent::Thinking {
                        thinking: "needs the tool".into(),
                        signature: None,
                    },
                    MessageContent::ToolUse {
                        id: "call_1".into(),
                        name: "node".into(),
                        input: r#"{"code":"print(1)"}"#.into(),
                        custom: false,
                        item_id: None,
                    },
                ],
                stop_reason: StopReason::ToolUse,
                usage: Some(Usage {
                    input_tokens: 12,
                    output_tokens: 3,
                }),
            }),
            Message::ToolResult(ToolResultMessage::error("call_1", "boom")),
        ];

        let json = serde_json::to_string(&history).unwrap();
        let parsed: Vec<Message> = serde_json::from_str(&json).unwrap();

        let [
            _,
            Message::Assistant(assistant),
            Message::ToolResult(result),
        ] = parsed.as_slice()
        else {
            panic!("history did not survive the round trip: {parsed:?}");
        };
        assert_eq!(assistant.stop_reason, StopReason::ToolUse);
        assert_eq!(assistant.usage.unwrap().input_tokens, 12);
        // The id is what lets a provider pair the result with the call.
        assert_eq!(
            parsed[1].tool_uses().map(|(id, ..)| id).collect::<Vec<_>>(),
            ["call_1"]
        );
        assert_eq!(result.tool_use_id, "call_1");
        assert!(result.is_error);
    }

    #[test]
    fn omits_defaults_from_the_wire_format() {
        let json = serde_json::to_string(&Message::ToolResult(ToolResultMessage::text(
            "call_1", "ok",
        )))
        .unwrap();
        assert!(!json.contains("isError"), "{json}");
        assert!(!json.contains("usage"), "{json}");
    }
}
