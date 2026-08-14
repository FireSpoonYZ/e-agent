//! Stateful extension used by the host's session-state tests.

use e_agent_extension::{Result, extension};

#[extension(
    description = "Remember values per session",
    system_prompt = "Use state_probe to remember values inside one session."
)]
mod state_probe {
    use super::*;

    #[state]
    #[derive(Default)]
    struct Remembered {
        values: Vec<String>,
    }

    #[tool]
    /// Remember one value for the current session and return the whole list.
    async fn remember(
        #[state] state: &mut Remembered,
        #[desc("Value to remember")] value: String,
    ) -> Result<String> {
        state.values.push(value);
        Ok(state.values.join(","))
    }

    #[tool]
    /// List every value remembered in the current session.
    async fn recall(#[state] state: &Remembered) -> Result<String> {
        Ok(state.values.join(","))
    }
}
