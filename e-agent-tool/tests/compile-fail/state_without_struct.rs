use e_agent_tool::{Result, extension};

#[extension(description = "no state struct")]
mod broken {
    use super::*;

    #[tool]
    /// Needs state that does not exist.
    pub async fn touch(#[state] state: &u8) -> Result<u8> {
        Ok(*state)
    }
}

fn main() {}
