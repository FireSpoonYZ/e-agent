use e_agent_tool::{Result, extension};

#[extension(description = "wrong state type")]
mod broken {
    use super::*;

    #[state]
    #[derive(Default)]
    pub struct Store {}

    #[derive(Default)]
    pub struct Other {}

    #[tool]
    /// Takes the wrong state type.
    pub async fn touch(#[state] state: &Other) -> Result<u8> {
        let _ = state;
        Ok(0)
    }
}

fn main() {}
