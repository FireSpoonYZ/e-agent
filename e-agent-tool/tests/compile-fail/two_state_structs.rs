use e_agent_tool::extension;

#[extension(description = "two states")]
mod broken {
    #[state]
    #[derive(Default)]
    pub struct First {}

    #[state]
    #[derive(Default)]
    pub struct Second {}
}

fn main() {}
