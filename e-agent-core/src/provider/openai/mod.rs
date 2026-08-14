use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        chat::ReasoningEffort,
        responses::{
            CreateResponseArgs, CustomGrammarFormatParam, CustomToolCallOutput,
            CustomToolCallOutputOutput, CustomToolParam, CustomToolParamFormat, EasyInputMessage,
            FunctionCallOutput, FunctionCallOutputItemParam, FunctionTool, FunctionToolCall,
            GrammarSyntax, InputContent, InputItem, InputParam, InputTextContent, Item, OutputItem,
            OutputMessageContent, ReasoningArgs, ReasoningSummary, Response, Role, Status,
            SummaryPart, Tool,
        },
    },
};

use super::Provider;
use crate::message::{
    AssistantMessage, Message, MessageContent, StopReason, ToolDef, ToolInput, Usage,
};
use anyhow::{Context, Result, bail};

pub struct OpenAIProvider {
    client: Client<OpenAIConfig>,
}

impl OpenAIProvider {
    pub fn new() -> Self {
        let http_client = reqwest::ClientBuilder::new()
            .user_agent("codex_cli_rs/0.125.0")
            .build()
            .unwrap();
        let client = Client::new().with_http_client(http_client);

        Self { client }
    }
}

impl Default for OpenAIProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    type Error = anyhow::Error;
    async fn send(
        &self,
        model: &str,
        context: crate::message::Context<'_>,
    ) -> Result<AssistantMessage> {
        let mut request = CreateResponseArgs::default();
        let (model, effort) = model.split_once(":").unwrap_or((model, "none"));
        let effort = serde_json::from_str::<ReasoningEffort>(&format!(r#""{}""#, effort))
            .context("reasoning effort format error")?;
        let reasoning = ReasoningArgs::default()
            .effort(effort)
            .summary(ReasoningSummary::Detailed)
            .build()
            .context("reasoning effort build failed")?;
        request
            .model(model)
            .reasoning(reasoning)
            .input(InputParam::Items(to_input_items(context.messages)))
            .store(false);
        if let Some(system_prompt) = context.system_prompt {
            request.instructions(system_prompt);
        }
        if !context.tools.is_empty() {
            request.tools(context.tools.iter().map(to_tool).collect::<Vec<_>>());
        }

        let response = self.client.responses().create(request.build()?).await?;
        from_response(response)
    }
}

fn to_tool(tool: &ToolDef) -> Tool {
    match &tool.input {
        ToolInput::Json(parameters) => Tool::Function(FunctionTool {
            name: tool.name.clone(),
            description: Some(tool.description.clone()),
            parameters: Some(parameters.clone()),
            strict: Some(false),
            defer_loading: None,
        }),
        input => Tool::Custom(CustomToolParam {
            name: tool.name.clone(),
            description: Some(tool.description.clone()),
            format: match input {
                ToolInput::Text => CustomToolParamFormat::Text,
                ToolInput::Lark(definition) => {
                    CustomToolParamFormat::Grammar(CustomGrammarFormatParam {
                        definition: definition.clone(),
                        syntax: GrammarSyntax::Lark,
                    })
                }
                ToolInput::Json(_) => unreachable!(),
            },
            defer_loading: None,
        }),
    }
}

/// Flatten the message history into Responses input items.
///
/// One `Message` can expand into several items: the API wants `function_call` and
/// `function_call_output` as top-level items, not as parts of a message.
/// `Thinking` blocks are dropped — replaying reasoning would need
/// `include: ["reasoning.encrypted_content"]` and a `signature` we never populate.
fn to_input_items(messages: &[Message]) -> Vec<InputItem> {
    let mut items = Vec::with_capacity(messages.len());

    for message in messages {
        let role = match message {
            Message::User(_) => Role::User,
            Message::Assistant(_) => Role::Assistant,
            // A tool result is a top-level item, not a message with a role.
            Message::ToolResult(result) => {
                let output = result
                    .content
                    .iter()
                    .filter_map(to_input_content)
                    .collect::<Vec<_>>();
                items.push(InputItem::Item(if result.custom {
                    Item::CustomToolCallOutput(CustomToolCallOutput {
                        call_id: result.tool_use_id.clone(),
                        output: CustomToolCallOutputOutput::List(output),
                        id: None,
                    })
                } else {
                    Item::FunctionCallOutput(FunctionCallOutputItemParam {
                        call_id: result.tool_use_id.clone(),
                        output: FunctionCallOutput::Content(output),
                        id: None,
                        status: None,
                    })
                }));
                continue;
            }
        };

        // Consecutive text blocks collapse into one message item, but a tool call in
        // between has to flush first so the original ordering survives.
        let mut text = String::new();
        let flush = |text: &mut String, items: &mut Vec<InputItem>| {
            if !text.is_empty() {
                items.push(InputItem::EasyMessage(EasyInputMessage {
                    role,
                    content: std::mem::take(text).into(),
                    ..Default::default()
                }));
            }
        };

        for content in message.content() {
            match content {
                MessageContent::Text { text: chunk } => text.push_str(chunk),
                MessageContent::ToolUse {
                    id,
                    name,
                    input,
                    custom,
                    item_id,
                } => {
                    flush(&mut text, &mut items);
                    items.push(InputItem::Item(if *custom {
                        Item::CustomToolCall(
                            serde_json::from_value(serde_json::json!({
                                "type": "custom_tool_call",
                                "call_id": id,
                                "input": input,
                                "name": name,
                                "id": item_id,
                            }))
                            .expect("valid custom tool call"),
                        )
                    } else {
                        Item::FunctionCall(FunctionToolCall {
                            arguments: input.clone(),
                            call_id: id.clone(),
                            name: name.clone(),
                            namespace: None,
                            id: None,
                            status: None,
                        })
                    }));
                }
                MessageContent::Thinking { .. } => {}
            }
        }

        flush(&mut text, &mut items);
    }

    items
}

/// Tool results only carry text today; anything else is not representable here.
fn to_input_content(content: &MessageContent) -> Option<InputContent> {
    match content {
        MessageContent::Text { text } => Some(InputContent::InputText(InputTextContent {
            text: text.clone(),
        })),
        _ => None,
    }
}

/// Collapse one response into a single assistant message, keeping item order.
fn from_response(response: Response) -> Result<AssistantMessage> {
    // A failed or cancelled response has no usable turn in it, so it is an error
    // rather than an empty assistant message the loop would silently accept.
    if matches!(response.status, Status::Failed | Status::Cancelled) {
        let detail = response
            .error
            .map(|error| error.message)
            .or_else(|| response.incomplete_details.map(|details| details.reason))
            .unwrap_or_else(|| "no detail reported".to_string());
        bail!("response {:?}: {detail}", response.status);
    }

    let mut content = Vec::new();
    let mut refused = false;

    for item in response.output {
        match item {
            OutputItem::Message(message) => {
                for part in message.content {
                    match part {
                        OutputMessageContent::OutputText(text) => {
                            content.push(MessageContent::Text { text: text.text });
                        }
                        // A refusal is the model's answer, so it is surfaced as text
                        // rather than dropped — but the stop reason records it.
                        OutputMessageContent::Refusal(refusal) => {
                            refused = true;
                            content.push(MessageContent::Text {
                                text: refusal.refusal,
                            });
                        }
                    }
                }
            }
            OutputItem::FunctionCall(call) => {
                content.push(MessageContent::ToolUse {
                    id: call.call_id,
                    name: call.name,
                    input: call.arguments,
                    custom: false,
                    item_id: call.id,
                });
            }
            OutputItem::CustomToolCall(call) => {
                content.push(MessageContent::ToolUse {
                    id: call.call_id,
                    name: call.name,
                    input: call.input,
                    custom: true,
                    item_id: Some(call.id),
                });
            }
            OutputItem::Reasoning(reasoning) => content.push(MessageContent::Thinking {
                thinking: reasoning
                    .summary
                    .into_iter()
                    .fold(String::new(), |total, s| {
                        format!(
                            "{}{}",
                            total,
                            match s {
                                SummaryPart::SummaryText(summary_text_content) =>
                                    summary_text_content.text,
                            }
                        )
                    }),
                signature: None,
            }),
            // Reasoning items are dropped: see `to_input_items`.
            _ => {}
        }
    }

    // if content.is_empty() {
    //     return Ok(Vec::new());
    // }

    let wants_tools = content
        .iter()
        .any(|block| matches!(block, MessageContent::ToolUse { .. }));
    // `incomplete` means the output was cut short, most often by the token limit.
    // The loop has to know, or it reports a truncated answer as a finished one.
    let stop_reason = if response.status == Status::Incomplete {
        StopReason::Length
    } else if wants_tools {
        StopReason::ToolUse
    } else if refused {
        StopReason::Refusal
    } else {
        StopReason::Stop
    };

    Ok(AssistantMessage {
        content,
        stop_reason,
        usage: response.usage.map(|usage| Usage {
            input_tokens: u64::from(usage.input_tokens),
            output_tokens: u64::from(usage.output_tokens),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolResultMessage;

    fn response(status: Status, output: Vec<OutputItem>) -> Response {
        Response {
            created_at: 0,
            id: "resp_1".into(),
            model: "gpt-5.6-sol".into(),
            object: "response".into(),
            output,
            status,
            parallel_tool_calls: None,
            background: None,
            billing: None,
            completed_at: None,
            conversation: None,
            error: None,
            incomplete_details: None,
            instructions: None,
            max_output_tokens: None,
            metadata: None,
            previous_response_id: None,
            prompt: None,
            prompt_cache_key: None,
            prompt_cache_retention: None,
            reasoning: None,
            safety_identifier: None,
            service_tier: None,
            temperature: None,
            text: None,
            tool_choice: None,
            tools: None,
            top_logprobs: None,
            top_p: None,
            truncation: None,
            usage: None,
        }
    }

    fn function_call(arguments: &str) -> OutputItem {
        OutputItem::FunctionCall(FunctionToolCall {
            arguments: arguments.into(),
            call_id: "call_1".into(),
            name: "node".into(),
            namespace: None,
            id: None,
            status: None,
        })
    }

    /// Ordering must survive the flatten: text before a tool call stays before it,
    /// text after it becomes a second message item, and a tool result is its own
    /// top-level item.
    #[test]
    fn flattens_text_tool_calls_and_results_in_order() {
        let messages = vec![
            Message::User(crate::message::UserMessage::text("hi")),
            Message::Assistant(AssistantMessage {
                content: vec![
                    MessageContent::Thinking {
                        thinking: "dropped".into(),
                        signature: None,
                    },
                    MessageContent::Text {
                        text: "before".into(),
                    },
                    MessageContent::ToolUse {
                        id: "call_1".into(),
                        name: "node".into(),
                        input: r#"{"code":"print(1)"}"#.into(),
                        custom: false,
                        item_id: None,
                    },
                    MessageContent::Text {
                        text: "after".into(),
                    },
                ],
                stop_reason: StopReason::ToolUse,
                usage: None,
            }),
            Message::ToolResult(ToolResultMessage::text("call_1", "1")),
        ];

        let items = serde_json::to_value(to_input_items(&messages)).unwrap();
        let items = items.as_array().unwrap();
        assert_eq!(items.len(), 5);
        assert_eq!(items[0]["content"], "hi");
        assert_eq!(items[1]["content"], "before");
        assert_eq!(items[2]["type"], "function_call");
        assert_eq!(items[2]["call_id"], "call_1");
        assert_eq!(items[3]["content"], "after");
        assert_eq!(items[4]["type"], "function_call_output");
        assert_eq!(items[4]["call_id"], "call_1");
        // Thinking never round-trips: no item carries it.
        assert!(!serde_json::to_string(items).unwrap().contains("dropped"));
    }

    #[test]
    fn maps_json_and_custom_tool_definitions_to_their_native_protocols() {
        let json_tool = serde_json::to_value(to_tool(&ToolDef {
            name: "lookup".into(),
            description: "Lookup a value".into(),
            input: ToolInput::Json(serde_json::json!({"type": "object"})),
        }))
        .unwrap();
        assert_eq!(json_tool["type"], "function");
        assert_eq!(json_tool["parameters"]["type"], "object");

        let text_tool = serde_json::to_value(to_tool(&ToolDef {
            name: "node".into(),
            description: "Run Python".into(),
            input: ToolInput::Text,
        }))
        .unwrap();
        assert_eq!(text_tool["type"], "custom");
        assert_eq!(text_tool["format"]["type"], "text");

        let lark_tool = serde_json::to_value(to_tool(&ToolDef {
            name: "node".into(),
            description: "Run Python".into(),
            input: ToolInput::Lark("start: /.+/".into()),
        }))
        .unwrap();
        assert_eq!(lark_tool["format"]["type"], "grammar");
        assert_eq!(lark_tool["format"]["syntax"], "lark");
    }

    #[test]
    fn accepts_custom_tool_call_input_as_raw_text() {
        let output = OutputItem::CustomToolCall(
            serde_json::from_value(serde_json::json!({
                "type": "custom_tool_call",
                "call_id": "call_1",
                "input": "print(\"a\\nb\")",
                "name": "node",
                "id": "ctc_1"
            }))
            .unwrap(),
        );
        let message = from_response(response(Status::Completed, vec![output])).unwrap();
        let MessageContent::ToolUse { input, custom, .. } = &message.content[0] else {
            panic!("expected custom tool call");
        };
        assert_eq!(input, "print(\"a\\nb\")");
        assert!(*custom);
    }

    #[test]
    fn replays_custom_calls_and_outputs_as_custom_items() {
        let messages = vec![
            Message::Assistant(AssistantMessage {
                content: vec![MessageContent::ToolUse {
                    id: "call_1".into(),
                    name: "node".into(),
                    input: "print(1)".into(),
                    custom: true,
                    item_id: Some("ctc_1".into()),
                }],
                stop_reason: StopReason::ToolUse,
                usage: None,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_use_id: "call_1".into(),
                content: vec![MessageContent::text("1")],
                is_error: false,
                custom: true,
            }),
        ];

        let items = serde_json::to_value(to_input_items(&messages)).unwrap();
        assert_eq!(items[0]["type"], "custom_tool_call");
        assert_eq!(items[0]["input"], "print(1)");
        assert_eq!(items[1]["type"], "custom_tool_call_output");
        assert_eq!(items[1]["call_id"], "call_1");
    }

    #[test]
    fn reports_tool_use_as_the_stop_reason() {
        let messages = from_response(response(
            Status::Completed,
            vec![function_call(r#"{"code":"1"}"#)],
        ))
        .unwrap();
        assert_eq!(messages.stop_reason, StopReason::ToolUse);
    }

    /// A truncated answer must not look like a finished one.
    #[test]
    fn reports_length_when_the_response_is_incomplete() {
        let output = vec![OutputItem::Message(
            serde_json::from_value(serde_json::json!({
                "content": [{"type": "output_text", "text": "half", "annotations": [], "logprobs": null}],
                "id": "msg_1",
                "role": "assistant",
                "status": "incomplete",
            }))
            .unwrap(),
        )];
        let messages = from_response(response(Status::Incomplete, output)).unwrap();
        assert_eq!(messages.stop_reason, StopReason::Length);
    }

    #[test]
    fn rejects_a_failed_response() {
        assert!(from_response(response(Status::Failed, Vec::new())).is_err());
    }

    #[test]
    fn leaves_function_arguments_for_the_executor_to_parse() {
        let message =
            from_response(response(Status::Completed, vec![function_call("not json")])).unwrap();
        let MessageContent::ToolUse { input, custom, .. } = &message.content[0] else {
            panic!("expected function tool call");
        };
        assert_eq!(input, "not json");
        assert!(!custom);
    }
}
