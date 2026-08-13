use anyhow::{Context, Result};
use e_agent_tool::SessionId;

use crate::{
    message::{Message, MessageContent, ToolResultMessage, UserMessage},
    provider::Provider,
    tool::ToolExecutor,
};

pub struct Session<P: Provider, E: ToolExecutor> {
    id: SessionId,
    system_prompt: String,
    messages: Vec<Message>,
    trun: usize,
    provider: P,
    tool_executor: E,
}

impl<P: Provider, E: ToolExecutor> Session<P, E> {
    pub fn new(provider: P, tool_executor: E, system_prompt: impl ToString) -> Self {
        Self {
            id: SessionId::next(),
            provider,
            tool_executor,
            system_prompt: system_prompt.to_string(),
            messages: Vec::new(),
            trun: 0,
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Release the per-session state held by every loaded extension.
    pub async fn close(&mut self) -> Result<()> {
        self.tool_executor
            .drop_session(self.id)
            .await
            .map_err(|err| anyhow::anyhow!("drop session state failed: {err:?}"))
    }

    pub fn build_system_prompt(&self) -> String {
        let mut prompt = format!(
            "{}\n当前时间为:{}\n当前目录为:{}",
            self.system_prompt,
            chrono::Local::now(),
            std::env::current_dir().unwrap().display()
        );
        for extension_prompt in self.tool_executor.system_prompts() {
            prompt.push('\n');
            prompt.push_str(&extension_prompt);
        }
        prompt
    }

    pub async fn run_one_trun(&mut self, user_input: UserMessage) -> Result<()> {
        self.trun += 1;
        self.messages.push(Message::User(user_input));

        println!("\n\n================ trun {} ================", self.trun);

        loop {
            let system_prompt = self.build_system_prompt();
            let tool_defs = self.tool_executor.tool_defs();
            let context = crate::message::Context {
                system_prompt: Some(&system_prompt),
                messages: &self.messages,
                tools: &tool_defs,
            };
            let model = std::env::var("E_MODULE_BIG").context("get model failed")?;
            let answer = match self.provider.send(&model, context).await {
                Ok(answer) => answer,
                Err(e) => {
                    println!("llm invoke failed: {e:?}");
                    break;
                }
            };

            print_message(&answer.content);

            println!("================================");

            self.messages.push(Message::Assistant(answer.clone()));

            // toolcall
            let mut will_stop_loop = true;
            for content in answer.content.into_iter() {
                if let MessageContent::ToolUse {
                    id,
                    name,
                    input,
                    custom,
                    ..
                } = content
                {
                    will_stop_loop = false;
                    let tool_result = match self.tool_executor.call(self.id, &name, input).await {
                        Ok(output) => ToolResultMessage {
                            tool_use_id: id,
                            content: output.content,
                            is_error: false,
                            custom,
                        },
                        Err(e) => {
                            let mut result = ToolResultMessage::error(id, format!("{e:?}"));
                            result.custom = custom;
                            result
                        }
                    };

                    print_message(&tool_result.content);

                    self.messages.push(Message::ToolResult(tool_result));
                }
            }

            if will_stop_loop {
                println!("tool call is empty, finish this trun");
                break;
            }

            println!("================================\n\n\n");
        }
        Ok(())
    }
}

fn print_message(content: &[MessageContent]) {
    for content in content.iter() {
        match content {
            crate::message::MessageContent::Text { text } => {
                println!("text: {text}");
            }
            crate::message::MessageContent::Thinking { thinking, .. } => {
                println!("thinking: {thinking}");
            }
            crate::message::MessageContent::ToolUse { name, input, .. } => {
                println!("tool use: {}\n{}", name, input);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use e_agent_tool::SessionId;

    use super::Session;
    use crate::{
        message::{AssistantMessage, Context, ToolDef},
        provider::Provider,
        tool::{ToolExecutor, ToolOutput},
    };

    struct NoProvider;

    #[async_trait::async_trait]
    impl Provider for NoProvider {
        type Error = anyhow::Error;
        async fn send(
            &self,
            _model: &str,
            _context: Context<'_>,
        ) -> Result<AssistantMessage, Self::Error> {
            unimplemented!("prompt assembly does not call the provider")
        }
    }

    /// Two extensions whose prompts must stay in load order.
    struct TwoExtensions;

    #[async_trait::async_trait]
    impl ToolExecutor for TwoExtensions {
        type Error = anyhow::Error;
        fn tool_defs(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        fn system_prompts(&self) -> Vec<String> {
            vec![
                "first extension prompt".into(),
                "second extension prompt".into(),
            ]
        }
        async fn call(
            &self,
            _session: SessionId,
            _name: &str,
            _input: String,
        ) -> Result<ToolOutput, Self::Error> {
            unimplemented!("prompt assembly does not call tools")
        }
        async fn drop_session(&self, _session: SessionId) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// Base prompt, runtime context, then extension prompts in load order.
    #[test]
    fn assembles_prompt_in_deterministic_order() {
        let session = Session::new(NoProvider, TwoExtensions, "base prompt");
        let prompt = session.build_system_prompt();
        let lines: Vec<_> = prompt.lines().collect();

        assert_eq!(lines[0], "base prompt");
        assert!(lines[1].starts_with("当前时间为:"));
        assert!(lines[2].starts_with("当前目录为:"));
        assert_eq!(
            &lines[3..],
            ["first extension prompt", "second extension prompt"]
        );
    }

    /// Every session gets its own identity, and closing one is not an error.
    #[test]
    fn assigns_unique_session_ids() {
        let mut first = Session::new(NoProvider, TwoExtensions, "base");
        let second = Session::new(NoProvider, TwoExtensions, "base");
        assert_ne!(first.id(), second.id());

        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(first.close())
            .unwrap();
    }
}
