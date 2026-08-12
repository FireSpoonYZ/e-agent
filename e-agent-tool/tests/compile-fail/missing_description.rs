use e_agent_tool::extension;

#[extension(system_prompt = "no description")]
mod broken {}

fn main() {}
