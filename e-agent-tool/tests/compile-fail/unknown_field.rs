use e_agent_tool::extension;

#[extension(description = "ok", prompt_snippet = "nope")]
mod broken {}

fn main() {}
