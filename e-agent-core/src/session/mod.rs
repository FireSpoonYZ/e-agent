mod error;

use anyhow::Result;

use crate::{
    message::{Message, ToolResultMessage, UserMessage},
    provider::Provider,
    tool::ToolExecutor,
};

pub struct Session<P: Provider, E: ToolExecutor> {
    system_prompt: String,
    messages: Vec<Message>,
    trun: usize,
    provider: P,
    tool_executor: E,
}

impl<P: Provider, E: ToolExecutor> Session<P, E> {
    pub fn new(provider: P, tool_executor: E, system_prompt: impl ToString) -> Self {
        Self {
            provider,
            tool_executor,
            system_prompt: system_prompt.to_string(),
            messages: Vec::new(),
            trun: 0,
        }
    }

    pub fn build_system_prompt(&self) -> String {
        format!(
            "{}\n当前时间为:{}\n当前目录为:{}",
            self.system_prompt,
            chrono::Local::now(),
            std::env::current_dir().unwrap().display()
        )
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
            let answer = match self.provider.send("gpt-5.6-sol", context).await {
                Ok(answer) => answer,
                Err(e) => {
                    println!("llm invoke failed: {e:?}");
                    break;
                }
            };

            self.messages.push(Message::Assistant(answer.clone()));

            // toolcall
            let mut tc = Vec::new();
            for content in answer.content.into_iter() {
                match content {
                    crate::message::MessageContent::Text { text } => {
                        println!("say: {text}");
                    }
                    crate::message::MessageContent::Thinking { thinking, .. } => {
                        println!("thinking: {thinking}");
                    }
                    crate::message::MessageContent::ToolUse {
                        id,
                        name,
                        input,
                        custom,
                        ..
                    } => {
                        println!("tool use({}): {}({:?})", id, name, input);
                        tc.push((id, name, input, custom));
                    }
                }
            }

            if tc.is_empty() {
                println!("tool call is empty, finish this trun");
                break;
            }

            for (id, name, input, custom) in tc.into_iter() {
                let tool_result = match self.tool_executor.call(&name, input).await {
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

                println!("tool_result: {:?}", tool_result);

                self.messages.push(Message::ToolResult(tool_result));
            }

            println!("================================");
        }
        Ok(())
    }
}
