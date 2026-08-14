use e_agent_extension::{Result, extension};

#[extension(description = "two state params")]
mod broken {
    use super::*;

    #[state]
    #[derive(Default)]
    pub struct Store {}

    #[tool]
    /// Takes state twice.
    pub async fn touch(#[state] a: &Store, #[state] b: &Store) -> Result<u8> {
        let _ = (a, b);
        Ok(0)
    }
}

fn main() {}
