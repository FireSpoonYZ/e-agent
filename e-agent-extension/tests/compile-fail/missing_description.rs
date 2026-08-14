use e_agent_extension::extension;

#[extension(system_prompt = "no description")]
mod broken {}

fn main() {}
