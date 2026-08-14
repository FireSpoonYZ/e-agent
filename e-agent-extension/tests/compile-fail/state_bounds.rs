use e_agent_extension::{Result, extension};

#[extension(description = "state must be Default + Send + Sync")]
mod broken {
    use super::*;

    #[state]
    pub struct NoDefault {
        pub cell: std::rc::Rc<u8>,
    }

    #[tool]
    /// Touch the state.
    pub async fn touch(#[state] state: &NoDefault) -> Result<u8> {
        Ok(*state.cell)
    }
}

fn main() {}
