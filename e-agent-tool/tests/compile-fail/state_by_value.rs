use e_agent_tool::{Result, extension};

#[extension(description = "state by value")]
mod broken {
    use super::*;

    #[state]
    #[derive(Default)]
    pub struct Store {}

    #[tool]
    /// Takes state by value.
    pub async fn touch(#[state] state: Store) -> Result<u8> {
        let _ = state;
        Ok(0)
    }
}

fn main() {}
